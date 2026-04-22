# PTY Threading v2: Inline-Feed Three-Thread Architecture

> Supersedes the threading model in [04-pty-threading.md](04-pty-threading.md).
> **IMPORTANT:** IO and signaling model superseded by [15-ntdll-io-rework.md](15-ntdll-io-rework.md).
> Thread structure, PTY split, shutdown sequence, and HPCON serialization remain valid.
> **IMPORTANT:** The end-state Windows architecture is superseded by
> [16-in-process-conpty.md](16-in-process-conpty.md), which removes the
> external ConPTY transport entirely.

## Goal

Eliminate the `PtyEvent::Output` channel crossing on the hot path by
having the read thread call `terminal.feed()` inline, and rework the IO
thread into a writer/timer thread mirroring Ghostty's `Thread.zig`.

## Motivation

The current architecture sends every PTY read through a bounded
`async_channel` to a separate IO thread that locks the terminal and
feeds bytes. This channel sit on the hottest path for IO throughput —
every byte of shell output crosses a thread boundary, pays for an
allocation (`OutputBuffer`), and wakes the IO thread.

Ghostty avoids this: its read thread calls `processOutput` inline,
locking the terminal mutex directly. The writer thread handles only cold
operations (PTY writes, timers, resize coalescing).

Additionally, our current overlapped IO model has a resize ordering bug:
when the resize command and a pending read complete simultaneously,
`WaitForMultipleObjects` processes resize first (lower index), causing
terminal grid resize before stale old-grid data is fed. This misplaces
the cursor.

## Thread Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        GPUI Main Thread                          │
│                                                                  │
│  ┌──────────────┐   ┌──────────────┐   ┌───────────────────────┐ │
│  │ signal_task  │   │  event_task  │   │  TerminalSession      │ │
│  │ await signal │   │ await IoEvent│   │  (user input, render) │ │
│  └──────▲───────┘   └──────▲───────┘   └───────────┬───────────┘ │
│         │                  │                       │             │
└─────────┼──────────────────┼───────────────────────┼─────────────┘
          │ signal_tx        │ event_tx              │ io_tx
          │ (cap 1)          │ (cap 64)              │ (crossbeam, cap 64)
          │                  │                       │
┌─────────┼──────────────────┼───────┐    ┌──────────▼──────────┐
│         │  Read Thread     │       │    │    IO Thread        │
│  ┌──────┴──────────────────┴────┐  │    │  (Writer/Timer)     │
│  │                              │  │    │                     │
│  │  ReadFile (overlapped, N)    │  │    │  io_rx.recv_timeout │
│  │  process buf[N-1]            │  │    │                     │
│  │  GORSTH(N) → lock terminal   │  │    │  IoMsg::Input       │
│  │  → feed() → drain_events()   │  │    │  → WriteFile conin  │
│  │  → unlock                    │  │    │                     │
│  │  ReadFile (overlapped, N+1)  │  │    │  IoMsg::Resize      │
│  │                              │  │    │  → coalesce (25ms)  │
│  │  Owns: conout, child,        │◄─╌╌╌╌╌┤  → signal notify    │
│  │    HPCON (resize only),      │notify │                     │
│  │    Arc<Mutex<Terminal>>      │  │    │  IoMsg::Close       │
│  │                              │  │    │  → signal notify    │
│  └──────────────────────────────┘  │    │                     │
│                                    │    │  IoMsg::SyncOutput  │
│  WFMO: [notify, child, read_a,     │    │  → 1s safety timer  │
│         read_b]                    │    │                     │
└────────────────────────────────────┘    │  Owns: conin, HPCON │
                                          │    (close only)     │
                                          └─────────────────────┘

GORSTH = GetOverlappedResultSameThread
```

### Ghostty Mapping

```
Ghostty                          Rustty
─────────────────────────────────────────────────────
Read thread (Exec.zig)     →    Read thread  (owns conout, inline feed)
IO thread  (Thread.zig)    →    IO thread    (owns conin, mailbox-driven)
Renderer thread            →    GPUI main thread  (renderer + app)
```

## PTY Split

The `Pty` struct is consumed via `split()` to divide ownership between
the two threads. Both halves hold the raw HPCON value (an `isize`) — it
is safe to share because resize and close are temporally exclusive
(resize stops before close begins).

```rust
pub struct PtyReader {
    pub conout: OwnedHandle,
    pub child: ChildProcess,
    pub hpcon: HPCON,
    pub resize_fn: ResizePseudoConsoleFn,
}

pub struct PtyWriter {
    pub conin: OwnedHandle,
    pub backend: Conpty,       // owns HPCON for close_async + drop
}
```

`PtyReader::resize()` calls the raw `resize_fn(hpcon, coord)`.
`PtyWriter::start_shutdown()` calls `backend.close_async()`, which
takes the HPCON from `Conpty.handle` — after this point, `PtyReader`'s
raw handle is stale, but no more resizes will be sent because the UI
has already sent `IoMsg::Close`.

### Drop Order

The session's `Drop` impl controls thread join order (see Shutdown
Sequence below). `PtyWriter.backend` drops before `PtyWriter.conin`,
matching the current `Pty` struct's field order requirement (HPCON
must close before pipe handles to avoid deadlock).

## Channels

| Name | Type | Cap | Producer(s) | Consumer |
|------|------|-----|-------------|----------|
| `io_tx`/`io_rx` | `crossbeam_channel::bounded<IoMsg>` | 64 | GPUI, read thread | IO thread |
| `signal_tx`/`signal_rx` | `async_channel::bounded<()>` | 1 | read thread, IO thread | GPUI async task |
| `event_tx`/`event_rx` | `async_channel::bounded<IoEvent>` | 64 | read thread | GPUI async task |

### Why `crossbeam-channel`

The IO thread needs `recv_timeout` for timer management (resize
coalesce, sync-output safety). `std::sync::mpsc` has `recv_timeout` but
`crossbeam-channel` is more robust, well-tested for bounded MPSC, and
`crossbeam-utils`/`crossbeam-queue` are already transitive dependencies.

### Read Thread Notify

The IO thread signals the read thread via a lightweight Win32 mechanism
that integrates with `WaitForMultipleObjects`:

```rust
struct ReadThreadNotify {
    event: HANDLE,                          // manual-reset Win32 event
    pending_resize: Mutex<Option<WindowSize>>,
    closing: AtomicBool,                    // shutdown flag
    hpcon_op: Mutex<()>,                    // serializes resize vs close
}
```

- IO thread writes to `pending_resize` or `closing`, then calls
  `SetEvent(event)`.
- Read thread includes `notify.event` in its WFMO array at index 0
  (highest priority). When signaled, it calls `ResetEvent`, then checks
  `closing` and `pending_resize`.
- The `pending_resize` Mutex is uncontended in practice — only touched
  on resize, which is rare.
- The `hpcon_op` Mutex serializes `ResizePseudoConsole` (read thread)
  against `ClosePseudoConsole` (IO thread's close background thread).
  See "HPCON Serialization" below.

## Message Types

```rust
enum IoMsg {
    /// User input bytes → WriteFile(conin).
    Input(Vec<u8>),
    /// Device response bytes → WriteFile(conin).
    /// Same pipe, same ordering, but semantically separate.
    Reply(Vec<u8>),
    /// Resize request. IO thread coalesces, then signals read thread.
    Resize(WindowSize),
    /// Begin/reset the 1-second synchronized output safety timer.
    StartSyncOutput,
    /// Ordered shutdown.
    Close,
}

enum IoEvent {
    Bell,
    TitleChanged(String),
    Exited(Option<ExitStatus>),
    Error(String),
}
```

## Read Thread (Hot Path)

### Double-Buffer Read-Ahead (Windows Terminal Pattern)

Two buffers (`buf_a`, `buf_b`) alternate roles. `ReadFile` is issued
before processing the previous read's data, so the OS can fill the next
buffer while we feed the terminal:

```
Iteration 0 (startup):
  ReadFile(buf_a)          → no previous data
  GORSTH(buf_a)            → data_a ready

Iteration 1:
  ReadFile(buf_b)          → process data_a: lock → feed → unlock
  GORSTH(buf_b)            → data_b ready

Iteration 2:
  ReadFile(buf_a)          → process data_b: lock → feed → unlock
  GORSTH(buf_a)            → data_a ready
  ...
```

### `GetOverlappedResultSameThread`

Avoids a kernel call on the fast path when the read already completed
inline. Since we guarantee single-thread use of each OVERLAPPED struct,
we can safely read `overlapped.Internal` directly:

```rust
fn get_overlapped_result_same_thread(
    overlapped: &OVERLAPPED,
) -> Result<u32, io::Error> {
    // Fast path: check if already completed (no kernel call).
    if overlapped.Internal == STATUS_PENDING as usize {
        // Slow path: wait for completion.
        let r = unsafe {
            WaitForSingleObjectEx(overlapped.hEvent, INFINITE, 0)
        };
        if r != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
    }
    // Safe to read: single-threaded, and either completed inline
    // or we just waited for hEvent.
    let hr = overlapped.Internal as i32;
    if hr < 0 {
        // NTSTATUS failure → translate to io::Error
        return Err(io::Error::from_raw_os_error(
            ntstatus_to_win32(hr) as i32,
        ));
    }
    Ok(overlapped.InternalHigh as u32)
}
```

### Read Completion Processing

After each read completes (via `GORSTH`):

```
1. Lock Mutex<Terminal>
2. terminal.feed(buf)
3. sync_before = was_synchronized (cached from prior iteration)
4. sync_after  = terminal.is_synchronized_output()
5. vt_events   = terminal.drain_events()
6. Unlock

7. For each VtEvent:
   - DeviceResponse → io_tx.send(IoMsg::Reply(bytes))
   - Bell           → event_tx.try_send(IoEvent::Bell)
   - TitleChanged   → event_tx.try_send(IoEvent::TitleChanged(..))

8. If !sync_before && sync_after:
   io_tx.send(IoMsg::StartSyncOutput)

9. If !sync_after:
   signal_tx.try_send(())   // renderer wakeup
```

### Resize Handling on Read Thread

When `notify.event` fires in WFMO:

```
1. ResetEvent(notify.event)
2. Check closing → if true, enter drain-to-EOF state (see Shutdown)
3. Take pending_resize → if Some(size):
   a. CancelIoEx(conout, &overlapped_a)  // cancel active read
   b. CancelIoEx(conout, &overlapped_b)  // cancel other buffer too
      (only for buffers in Pending state; tolerate ERROR_NOT_FOUND)
   c. For each Pending buffer:
      GetOverlappedResult(wait=TRUE)
      Tolerate ERROR_OPERATION_ABORTED / ERROR_BROKEN_PIPE
      If bytes > 0: lock terminal → feed(buf[..n]) → unlock
   d. Lock hpcon_op → if !closing: ResizePseudoConsole(hpcon, size) → unlock
   e. Lock terminal
   f. terminal.set_cell_size(size.cell_width, size.cell_height)
   g. terminal.resize(size.num_cols, size.num_lines)
   h. Unlock
   i. signal_tx.try_send(())   // repaint with new size
   j. Re-arm double-buffer reads (both buffers back to Idle → Pending)
```

This sequence ensures:
- Already-issued read completions are drained before the terminal is
  resized, fixing the WFMO index-priority cursor bug
- `ResizePseudoConsole` happens before `terminal.resize()`, so reflow
  output from ConPTY is fed into the correctly-sized grid. This does not
  matter in practice because no bytes are fed between ConPTY update and Ghostty update
- The `hpcon_op` lock prevents resize from racing with close

**Invariant to maintain:** No bytes are fed to Ghostty between ResizePseudoConsole and terminal.resize() (or vice versa).

**Per-buffer state tracking.** Each buffer tracks its state as
`Idle` or `Pending`. `CancelIoEx` and `GetOverlappedResult` are only
called on `Pending` buffers. After harvest, buffers return to `Idle`.

### WFMO Array

```
Index 0: notify.event       (resize/close signals from IO thread)
Index 1: child_handle        (child process exit)
Index 2: overlapped_a.hEvent (read buffer A completion)
Index 3: overlapped_b.hEvent (read buffer B completion)
```

When multiple events fire simultaneously, WFMO returns the lowest
index. This gives resize/close highest priority, child exit second,
and read completions lowest — exactly the right precedence.

### Child Exit

When `child_handle` signals:

```
1. Record exit status from child process
2. Set child_exited = true
3. Continue reading conout until EOF/BrokenPipe
   (ConPTY may still have buffered output after child exits)
4. When BrokenPipe/EOF:
   a. Drain any pending overlapped completions
   b. Lock terminal → feed remaining bytes → unlock
   c. event_tx.send_blocking(IoEvent::Exited(status))
   d. signal_tx.try_send(())
   e. Exit read loop
```

Key: child exit does NOT mean "stop reading." The read thread keeps
draining conout until the pipe closes (EOF/BrokenPipe), ensuring no
output is lost.

## IO Thread (Cold Path)

The IO thread mirrors Ghostty's `Thread.zig`. It blocks on
`crossbeam_channel::recv_timeout`, using the timeout for timer
management:

```rust
loop {
    let timeout = self.next_timer_deadline();
    match io_rx.recv_timeout(timeout) {
        Ok(msg)                              => self.handle_msg(msg),
        Err(RecvTimeoutError::Timeout)       => self.fire_timers(),
        Err(RecvTimeoutError::Disconnected)  => break,
    }
}
```

### Message Handling

| Message | Action |
|---------|--------|
| `Input(bytes)` | `WriteFile(conin)` in ≤64 KiB chunks (overlapped) |
| `Reply(bytes)` | `WriteFile(conin)` (same pipe, device responses) |
| `Resize(size)` | Store as `pending_resize` (last-wins). Start/reset 25ms coalesce timer. |
| `StartSyncOutput` | Start/reset 1-second safety timer. |
| `Close` | Set `notify.closing` + `SetEvent`. Drain remaining messages. Lock `hpcon_op` → `close_async()`. Exit loop. |

### Resize Coalesce

When the 25ms coalesce timer fires and `pending_resize` is `Some`:

```
1. Take the size from pending_resize
2. Store it in notify.pending_resize
3. SetEvent(notify.event)   // wake read thread
```

The actual `ResizePseudoConsole` + `terminal.resize()` happen on the
read thread (see above).

### Sync-Output Safety Timer

When the 1-second timer fires:

```
1. Lock terminal
2. terminal.reset_synchronized_output()  // clear DEC mode 2026
3. Unlock
4. signal_tx.try_send(())   // force render
```

### Overlapped Writes

`conin` was created with `FILE_FLAG_OVERLAPPED`. The IO thread uses
overlapped `WriteFile` with its own OVERLAPPED struct and manages a
write queue (`VecDeque<u8>`) with in-flight tracking, matching the
current worker thread's write logic. This avoids blocking on sluggish
shells.

After draining the mailbox, the IO thread signals a renderer wakeup
(like Ghostty's `io.renderer_wakeup.notify()` at the end of
`drainMailbox`).

## HPCON Serialization

Both the read thread (`ResizePseudoConsole`) and the IO thread's close
background thread (`ClosePseudoConsole`) operate on the raw HPCON value.
Message ordering alone does not prevent races — a resize may be in
flight when close starts. We serialize with `hpcon_op`:

**Read thread resize path:**
```
lock hpcon_op
if !closing.load():
    ResizePseudoConsole(hpcon, size)
unlock hpcon_op
```

**IO thread close path:**
```
closing.store(true)              // prevent new resizes
SetEvent(notify.event)           // wake read thread
lock hpcon_op                    // wait for any in-flight resize
close_async(hpcon)               // ClosePseudoConsole on background thread
unlock hpcon_op
```

The `closing` flag is checked inside the lock to prevent TOCTOU: once
the read thread holds `hpcon_op` and sees `closing == false`, it is
safe to call resize because close cannot start until the lock is
released.

## Shutdown Sequence

Correct ordering prevents hangs, zombie processes, and data loss.

**Critical ConPTY fact:** `ClosePseudoConsole` blocks until the conout
pipe is fully drained. The read thread MUST keep reading until
EOF/BrokenPipe, or close will hang.

```
 1. UI drops TerminalSession
 2. TerminalSession::drop() sends IoMsg::Close via io_tx
    (send_blocking — must not be lost on a full channel)
 3. IO thread receives Close
    a. Sets notify.closing = true
    b. SetEvent(notify.event)         // wake read thread
    c. Drains remaining IoMsg (flushes pending writes)
    d. Clears any pending resize (no more resizes after close)
    e. Locks hpcon_op → calls close_async() → unlock
       (spawns background thread calling ClosePseudoConsole)
    f. Exits IO loop
 4. ClosePseudoConsole sends CTRL_CLOSE_EVENT to child,
    then blocks until conout pipe is drained
 5. Read thread sees notify event (closing == true)
    a. Enters drain-to-EOF state: no more resizes, no IoMsg::Reply
    b. Continues reading conout until BrokenPipe/EOF
       (feeds terminal inline, signals renderer)
    c. On EOF: harvests pending overlapped completions
    d. Feeds any remaining bytes
    e. Emits IoEvent::Exited(status)
    f. Exits read loop
    ─── PtyReader drops here (conout + child handles closed) ───
 6. TerminalSession::drop() joins read thread
 7. TerminalSession::drop() drops io_tx → io_rx disconnects
    (IO thread already exited from step 3f)
 8. TerminalSession::drop() joins IO thread
    ─── PtyWriter drops here ───
 9. PtyWriter.backend drops → Conpty::drop() joins close_thread
    (ClosePseudoConsole completed because read thread drained conout)
10. PtyWriter.conin drops → conin pipe handle closed
```

Key invariants:
- Read thread is joined BEFORE IO thread. The read thread may send
  `IoMsg::Reply` to `io_tx` during normal operation; joining it first
  ensures it stops producing before the IO thread exits.
- Read thread **drains to EOF** — it does not exit on `closing` alone.
  This prevents `ClosePseudoConsole` from blocking forever.
- During drain-to-EOF, the read thread tolerates `io_tx` send failures
  (IO thread may have already exited). It skips `IoMsg::Reply` /
  `IoMsg::StartSyncOutput` once closing.
- Graceful shutdown timeout (1s): if conout doesn't reach EOF within
  1 second of `closing`, force-terminate the child via
  `TerminateProcess` (read thread owns the child handle).

## Data Flow Comparison

```
CURRENT:
  ReadFile → OutputBuffer → async_channel::send(PtyEvent::Output)
           → io-thread wakeup → lock Terminal → feed() → unlock
           → signal_tx.try_send(())

NEW:
  ReadFile(buf[N]) → process buf[N-1]:
    lock Terminal → feed() → drain_events() → unlock
    → io_tx.send(Reply/StartSyncOutput)      [if VT events]
    → event_tx.try_send(Bell/TitleChanged)   [if side effects]
    → signal_tx.try_send(())                 [always, cap-1]
  GORSTH(buf[N]) → data ready
  ReadFile(buf[N+1]) → process buf[N] ...
```

The channel crossing + thread wakeup + `OutputBuffer` allocation are
removed from the hot path entirely.

## File Map

### New Files
- `crates/terminal/src/read_thread.rs` — read thread implementation
- `crates/terminal/src/write_thread.rs` — IO/writer thread (replaces `io_thread.rs`)

### Deleted
- `crates/pty/src/buffer_pool.rs` — replaced by stack buffers
- `crates/pty/src/handle.rs` — thread orchestration moves to terminal crate
- `crates/terminal/src/io_thread.rs` — replaced by `write_thread.rs`

### Modified
- `crates/pty/src/lib.rs` — remove `PtyEvent`, `PtyCommand`, `BufferPool`, channel constants
- `crates/pty/src/windows/mod.rs` — add `PtyReader`, `PtyWriter`, `Pty::split()`
- `crates/pty/src/windows/conpty.rs` — expose raw HPCON + resize fn for `PtyReader`
- `crates/terminal/src/types.rs` — add `IoMsg`, `ReadThreadNotify`; flatten `IoEvent`
- `crates/terminal/src/session.rs` — rewire with new channels, thread spawning, drop order
- `crates/terminal/src/lib.rs` — update module declarations

## Dependency

Add `crossbeam-channel` as a workspace dependency. It provides bounded
MPSC with `recv_timeout`, needed by the IO thread for timer-driven
coalescing. `crossbeam-utils` and `crossbeam-queue` are already
transitive deps in the lockfile.

## Risks and Mitigations

**Shared HPCON between threads.** Both `PtyReader` (resize) and
`PtyWriter` (close) hold the raw HPCON value. Serialized via
`hpcon_op` Mutex + `closing` AtomicBool (see "HPCON Serialization").

**Mutex contention on terminal.** The read thread and GPUI thread
compete for `Mutex<Terminal>`. The read thread holds it for
`feed() + drain_events()`, the GPUI thread holds it for
`RenderSnapshot::capture()`. Under heavy output, the GPUI thread may
experience brief delays. This matches Ghostty's model (same mutex
pattern). Mitigation: keep the lock scope minimal — no allocations or
IO inside the lock.

**Shutdown hang if read thread exits early.** If the read thread exits
on `closing` without draining to EOF, `ClosePseudoConsole` blocks
forever. Mitigated by the drain-to-EOF rule + force-terminate timeout.

**IoMsg::Close delivery.** If `io_tx` is full when `Close` is sent,
shutdown stalls. Mitigated by using `send_blocking` (not `try_send`)
for `Close`, so `TerminalSession::drop` blocks until the message is
delivered.

**`crossbeam-channel` is a new direct dependency.** It is
well-maintained, has zero unsafe in the bounded channel path, and is
already in the dep graph transitively. Low risk.
