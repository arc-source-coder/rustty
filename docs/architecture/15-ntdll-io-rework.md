# PTY Threading v3: ntdll APC-Based IO

> Supersedes the IOCP/OVERLAPPED model in [11-pty-threading-v2.md](11-pty-threading-v2.md).

## Motivation

Profiling shows `GetQueuedCompletionStatusEx` (GQCS) as the primary
bottleneck on the read thread — we are not saturating Ghostty's VT
parser. IOCP carries inherent overhead:

1. **Kernel object management** — the completion port has internal locks
2. **Completion packet queuing** — every IO posts a packet, queues it,
   wakes the port
3. **The OVERLAPPED tax** — `ReadFile`/`WriteFile` wrap `NtReadFile`/
   `NtWriteFile`, adding Event management, `GetLastError` TLS access,
   and BOOL return semantics that hide the actual NTSTATUS
4. **Our normalization overhead** — `SetFileCompletionNotificationModes`
   + manual `PostQueuedCompletionStatus` for sync completions exists
   solely to merge two completion paths into one

The Zig standard library (PR 31136, devlog 2026-02-03) demonstrates
that replacing kernel32 IOCP with ntdll APC-based IO eliminates these
costs. APC completions are delivered directly into the calling thread's
queue during alertable waits — no kernel object, no queue, no dequeue
syscall.

## Reference Material

- **Zig devlog**: "Bypassing Kernel32.dll for Fun and Nonprofit"
  (2026-02-03) — rationale and NtReadFile/NtWriteFile APC patterns
- **Zig PR 31136**: `lib/std/Io/Threaded.zig`, `lib/std/Build/Watch.zig`,
  `lib/std/os/windows/ntdll.zig` — concrete IOCP→APC migration
- **Windows Terminal**: `src/types/utils.cpp` L888 —
  `GetOverlappedResultSameThread` single-thread fast path (our new
  approach supersedes this entirely)

## Design Overview

Replace all kernel32 IO and signaling with ntdll equivalents:

| Current (kernel32)                    | New (ntdll)                          |
|---------------------------------------|--------------------------------------|
| `ReadFile`                            | `NtReadFile` (with APC)              |
| `WriteFile`                           | `NtWriteFile` (with APC)             |
| `OVERLAPPED`                          | `IO_STATUS_BLOCK`                    |
| `CreateIoCompletionPort`              | *(removed)*                          |
| `GetQueuedCompletionStatusEx`         | `NtDelayExecution(Alertable=TRUE)`   |
| `PostQueuedCompletionStatus`          | `NtAlertThread`                      |
| `GetOverlappedResult`                 | Direct IOSB status/information read  |
| `CancelIoEx`                          | `NtCancelIoFileEx`                   |
| `SetFileCompletionNotificationModes`  | *(removed)*                          |
| `CreateEventW` (write completion)     | *(removed)*                          |
| `CloseHandle` (event)                 | *(removed)*                          |
| `io::Error::last_os_error()`          | `NTSTATUS` return values             |
| `crossbeam_channel`                   | Lock-free queue + `NtAlertThread`    |
| `std::thread::spawn`                  | `NtCreateThreadEx`                   |

## Key Concepts

### IO_STATUS_BLOCK replaces OVERLAPPED

`OVERLAPPED` is a kernel32 fiction. The kernel operates on
`IO_STATUS_BLOCK` (16 bytes: Status + Information). No `hEvent`, no
Offset fields, no Internal/InternalHigh reinterpretation. The kernel
writes completion status directly into the IOSB.

```rust
#[repr(C)]
struct IoStatusBlock {
    status: NTSTATUS,       // or *mut c_void (union, but we only use status)
    information: usize,     // bytes transferred
}
```

### APC-based completion replaces IOCP

`NtReadFile` and `NtWriteFile` accept an `ApcRoutine` parameter. When
the IO completes, the kernel queues the APC to the issuing thread. The
APC fires the next time the thread enters an **alertable wait** — in
our case, `NtDelayExecution(Alertable=TRUE, ...)`.

We use Zig's `flagApc` pattern: the APC callback simply sets
`done = true` on a plain `bool` associated with the buffer. The main
loop checks the flag after waking.

This mirrors Zig naming and behavior (`flagApc`,
`waitForApcOrAlert`) intentionally. In Rust we use snake_case names
for the same concepts (`flag_apc`, `wait_for_apc_or_alert`).

```rust
unsafe extern "system" fn read_apc(
    context: *mut c_void,
    _iosb: *mut IoStatusBlock,
    _reserved: u32,
) {
    let done = &mut *(context as *mut bool);
    *done = true;
}
```

This is simpler than a per-buffer dispatch APC because the read thread
is single-threaded and owns all buffers — it can poll each buffer's
flag after waking. The APC's only job is to wake us from
`NtDelayExecution`.

### NtDelayExecution is the core wait primitive

`NtDelayExecution(Alertable=TRUE, &interval)` replaces GQCS as the
main blocking point. It returns:

| NTSTATUS         | Meaning                                           |
|------------------|---------------------------------------------------|
| `STATUS_SUCCESS` | Timeout expired                                   |
| `STATUS_ALERTED` | Woken by `NtAlertThread` (cross-thread signal)    |
| `STATUS_USER_APC`| An APC was delivered (IO completion)               |

All three are normal wake conditions in our loop. The return value is
exposed directly — no abstraction layer wrapping it into an enum.

### NtAlertThread replaces PostQueuedCompletionStatus

Cross-thread signaling (IO thread → read thread for resize/close) uses
`NtAlertThread(thread_handle)`. This sets the target thread's "alerted"
flag, causing any current or next alertable wait to return
`STATUS_ALERTED`.

**Important**: `NtAlertThreadByThreadId` is a completely unrelated
mechanism (it wakes `NtWaitForAlertByThreadId`, not alertable waits).
We use `NtAlertThread`, which takes a thread HANDLE, not a thread ID.

### NtCreateThreadEx replaces std::thread::spawn

`NtCreateThreadEx` returns a thread HANDLE directly with
`MAXIMUM_ALLOWED` access rights, which includes `THREAD_ALERT`. This
eliminates the need for a separate `NtOpenThread` call to obtain an
alertable handle.

```rust
NtCreateThreadEx(
    &mut handle,            // ← returned with THREAD_ALERT access
    MAXIMUM_ALLOWED,
    &object_attributes,
    current_process,
    entry_fn,
    context,
    CREATE_SUSPENDED,       // store handle before thread runs
    ...
)
// Store handle in shared state
notify.read_thread_handle = handle;
NtResumeThread(handle, null);
```

Thread join uses `NtWaitForSingleObject` + `NtClose`. This gives us
full ownership of the thread lifecycle with no `JoinHandle` wrapper.

### Lock-free bounded queue replaces crossbeam_channel

The IO thread's `crossbeam_channel::recv_timeout` loop is replaced by
a lock-free bounded MPMC queue (`crossbeam_queue::ArrayQueue`) +
`NtDelayExecution`. Producers push messages
and call `NtAlertThread(io_thread_handle)` to wake the IO thread.

The IO thread loop becomes:

```
loop {
    drain queue → handle messages
    fire timers
    flush writes (NtWriteFile with APC)
    NtDelayExecution(Alertable=TRUE, next_timer_interval)
    // Wakes on: timeout, alert (new message), or APC (write done)
}
```

This unifies message wakeup, timer expiry, and write completion into a
single wait point. Write APCs fire naturally during the same alertable
wait — no separate polling needed.

## Thread Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        GPUI Main Thread                          │
│                                                                  │
│  ┌──────────────┐   ┌───────────────┐  ┌───────────────────────┐ │
│  │ signal_task  │   │  event_task   │  │  TerminalSession      │ │
│  │ await signal │   │ await IoEvent │  │  (user input, render) │ │
│  └──────▲───────┘   └──────▲────────┘  └───────────┬───────────┘ │
│         │                  │                       │             │
└─────────┼──────────────────┼───────────────────────┼─────────────┘
          │ signal_tx        │ event_tx              │ queue.push()
          │ (cap 1)          │ (cap 64)              │ + NtAlertThread
          │                  │                       │
┌─────────┼──────────────────┼───────┐    ┌──────────▼──────────┐
│         │  Read Thread     │       │    │    IO Thread        │
│  ┌──────┴──────────────────┴────┐  │    │  (Writer/Timer)     │
│  │                              │  │    │                     │
│  │  NtReadFile (APC, ×4)        │  │    │  drain queue        │
│  │  NtDelayExecution(alert)     │  │    │  NtDelayExecution   │
│  │  harvest done bufs           │  │    │                     │
│  │  → lock terminal             │  │    │  Input              │
│  │  → feed() → drain_events()   │  │    │  → NtWriteFile+APC  │
│  │  → unlock                    │  │    │                     │
│  │  re-arm idle bufs            │  │    │  Resize             │
│  │                              │  │    │  → coalesce (25ms)  │
│  │  Owns: conout, child,        │◄╌╌╌╌╌ ┤  → NtAlertThread    │
│  │    HPCON (resize only),      │alert  │                     │
│  │    Arc<Mutex<Terminal>>      │  │    │  Close              │
│  │                              │  │    │  → NtAlertThread    │
│  └──────────────────────────────┘  │    │                     │
│                                    │    │  SyncOutput         │
│                                    │    │  → 1s safety timer  │
└────────────────────────────────────┘    │                     │
                                          │  Owns: conin, HPCON │
                                          │    (close only)     │
                                          └─────────────────────┘
```

## Read Thread

### 4-Buffer Pipeline

We increase from 2 to 4 in-flight read buffers. With APC delivery
being lower-overhead than IOCP, the extra pipeline depth keeps the
kernel filling buffers ahead of our processing. While we process
buffer 0, buffers 1–3 are in flight.

```rust
const NUM_READ_BUFS: usize = 4;
const READ_BUF_SIZE: usize = 64 * 1024;

struct ReadBuf {
    data: Box<[u8; READ_BUF_SIZE]>,
    iosb: IoStatusBlock,
    done: bool,
    state: BufState,           // Idle | InFlight
}
```

### Main Loop

```rust
loop {
    // 1. Issue NtReadFile on all idle buffers
    for buf in &mut bufs {
        if buf.state == Idle {
            buf.iosb = zeroed();
            buf.done = false;
            let status = NtReadFile(
                conout, null,
                read_apc, &buf.done as *mut _ as *mut _,
                &mut buf.iosb,
                buf.data.as_mut_ptr(), READ_BUF_SIZE as u32,
                null, null,
            );
            match status {
                STATUS_SUCCESS | STATUS_PENDING => buf.state = InFlight,
                STATUS_PIPE_BROKEN | STATUS_END_OF_FILE => { /* EOF */ },
                _ => { /* error */ },
            }
        }
    }

    // 2. Alertable wait — APCs fire here, setting done flags
    let timeout = compute_timeout(shutdown_deadline);
    let wake_status = NtDelayExecution(TRUE, &timeout);

    // 3. Harvest completed buffers
    for buf in &mut bufs {
        if buf.state == InFlight && buf.done {
            let n = buf.iosb.information;
            let status = buf.iosb.status;
            match status {
                STATUS_SUCCESS if n > 0 => {
                    feed_and_dispatch(&buf.data[..n], ...);
                }
                STATUS_END_OF_FILE | STATUS_PIPE_BROKEN => { /* EOF */ }
                STATUS_CANCELLED => { /* expected during resize/shutdown */ }
                _ => { /* error */ }
            }
            buf.state = Idle;  // will be re-armed in step 1
        }
    }

    // 4. Check cross-thread signals
    if notify.closing.load(Acquire) && !draining {
        draining = true;
        shutdown_deadline = Some(Instant::now() + SHUTDOWN_TIMEOUT);
    }
    if let Some(size) = notify.take_resize() {
        handle_resize(...);
    }
}
```

### Resize Path (Simplified)

With APC, resize no longer needs the complex `drain_iocp_for_buffers`
function. The resize path becomes:

```
1. NtCancelIoFileEx(conout, &buf.iosb) for each InFlight buffer
2. remaining = number of InFlight buffers
3. while remaining > 0:
     a. waitForApcOrAlert()   // blocking alertable wait; no busy spin
     b. harvest done buffers
     c. for each harvested buffer:
          - STATUS_SUCCESS + bytes>0 => feed terminal
          - STATUS_CANCELLED         => expected
          - STATUS_PIPE_BROKEN/EOF   => EOF handling
          - mark buffer Idle
          - remaining -= 1
4. ResizePseudoConsole (under hpcon_op lock)
5. terminal.resize()
6. Re-arm all buffers (step 1 of main loop handles this)
```

No `drain_iocp_for_buffers`. No `get_overlapped_result_iocp`. No
`OVERLAPPED_ENTRY` array. The "drain" is just an alertable wait with
completion-driven harvest; no zero-timeout spin loop.

### Synchronous Completion Fast Path

When `NtReadFile` returns `STATUS_SUCCESS` (not `STATUS_PENDING`), the
data is already in the buffer and `iosb.information` has the byte
count. The APC is still queued. We set `buf.state = InFlight` and let
the normal harvest loop process it after the next `NtDelayExecution`
(which returns immediately because the APC is already queued).

This naturally creates the equivalent of Windows Terminal's
`GetOverlappedResultSameThread` fast path — but without the
OVERLAPPED wrapper or Internal field polling.

## IO Thread

### Queue-Driven Loop

```rust
struct IoThread {
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    signal_tx: Sender<()>,
    renderer_tx: Option<crossbeam_channel::Sender<RendererMessage>>,

    // Message queue (replaces crossbeam_channel)
    msg_queue: Arc<ArrayQueue<IoMsg>>, // lock-free bounded queue
    wake_armed: AtomicBool,        // coalesces NtAlertThread wakeups

    // Write state
    write_queue: VecDeque<WriteChunk>,
    coalesce_buf: Option<BytesMut>,
    write_iosb: IoStatusBlock,
    write_done: bool,
    write_pending: bool,

    // Timers
    resize_deadline: Option<Instant>,
    pending_resize: Option<WindowSize>,
    sync_output_deadline: Option<Instant>,
}
```

Main loop:

```rust
loop {
    // Drain all pending messages
    while let Some(msg) = self.msg_queue.pop() {
        match msg {
            IoMsg::Input(bytes) | IoMsg::InputInline { .. } => self.enqueue_bytes(..),
            IoMsg::Reply(bytes) => self.enqueue_bytes(..),
            IoMsg::Resize(size) => { self.pending_resize = Some(size); ... },
            IoMsg::Scroll(op) => self.apply_scroll(op),
            IoMsg::StartSyncOutput => { ... },
            IoMsg::AttachRenderer(tx) => { self.renderer_tx = Some(tx); },
            IoMsg::DetachRenderer => { self.renderer_tx = None; },
            IoMsg::Close => { self.handle_close(); return; },
        }
    }

    self.fire_timers();
    self.flush_writes();    // issues NtWriteFile with APC

    let timeout = self.next_timer_deadline();
    NtDelayExecution(TRUE, &timeout);
    // Wakes on: timeout, NtAlertThread (new message), or write APC
}
```

### Wake Coalescing (`wake_armed`)

Calling `NtAlertThread` on every push works but can cause avoidable
syscall churn under bursty producers. We coalesce wakeups with a single
armed flag:

```rust
impl IoThreadNotify {
    pub fn send(&self, msg: IoMsg) {
        self.queue.push(msg);
        if self.wake_armed
            .compare_exchange(false, true, AcqRel, Acquire)
            .is_ok()
        {
            unsafe { NtAlertThread(self.io_thread); }
        }
    }
}

fn io_loop(&mut self) {
    loop {
        // Drain all currently queued messages.
        while let Some(msg) = self.msg_queue.pop() {
            self.handle(msg);
        }

        // Allow next producer burst to arm exactly one wake.
        self.wake_armed.store(false, Release);

        // Close race: producer may have queued after drain but before store(false).
        if let Some(msg) = self.msg_queue.pop() {
            self.handle(msg);
            while let Some(msg) = self.msg_queue.pop() {
                self.handle(msg);
            }
            continue;
        }

        self.fire_timers();
        self.flush_writes();
        NtDelayExecution(TRUE, &self.next_timer_deadline());
    }
}
```

This keeps wake behavior edge-triggered per burst rather than
message-triggered.

### Write Completion

`WRITE_POLL` is removed in this model. The old 5ms polling path is no
longer needed because `NtDelayExecution(Alertable=TRUE, ...)` wakes on
write APC completion directly.

`NtWriteFile` with APC sets `write_done = true` on completion. The
`flush_writes` loop checks this flag:

```rust
fn flush_writes(&mut self) {
    // Check if in-flight write completed
    if self.write_pending && self.write_done {
        let bytes_written = self.write_iosb.information;
        self.advance_front(bytes_written);
        self.write_pending = false;
    }
    if self.write_pending { return; }

    // Issue next write
    self.flush_coalesce_into_queue();
    if let Some(front) = self.write_queue.front() {
        self.write_done = false;
        self.write_iosb = zeroed();
        let status = NtWriteFile(
            conin, null,
            write_apc, &self.write_done as *const _ as *mut _,
            &mut self.write_iosb,
            front.ptr(), front.remaining() as u32,
            null, null,
        );
        match status {
            STATUS_SUCCESS => {
                // Completed synchronously — APC still queued, will
                // fire during next NtDelayExecution. Mark pending so
                // we process it through the normal path.
                self.write_pending = true;
            }
            STATUS_PENDING => { self.write_pending = true; }
            _ => { /* error, discard chunk */ }
        }
    }
}
```

## Cross-Thread Signaling

### ReadThreadNotify (v3)

```rust
pub struct ReadThreadNotify {
    /// Read thread handle (from NtCreateThreadEx, THREAD_ALERT access).
    pub read_thread: HANDLE,
    /// Pending resize. Written by IO thread, read by read thread.
    pub pending_resize: Mutex<Option<WindowSize>>,
    /// Shutdown flag.
    pub closing: AtomicBool,
    /// Serializes ResizePseudoConsole vs ClosePseudoConsole.
    pub hpcon_op: Mutex<()>,
}

impl ReadThreadNotify {
    pub fn signal(&self) {
        unsafe { NtAlertThread(self.read_thread); }
    }
}
```

No IOCP handle. No `CloseHandle` in Drop (the thread handle is closed
when the `PlatformThread` is joined/dropped).

### IoThreadNotify

```rust
pub struct IoThreadNotify {
    /// IO thread handle (from NtCreateThreadEx, THREAD_ALERT access).
    pub io_thread: HANDLE,
    /// Lock-free bounded queue.
    pub queue: Arc<ArrayQueue<IoMsg>>,
    /// Wakeup coalescing flag.
    pub wake_armed: AtomicBool,
}

impl IoThreadNotify {
    /// Push a message and wake the IO thread.
    pub fn send(&self, msg: IoMsg) {
        // Lossless path for protocol-critical messages (Close/Reply): retry
        // until enqueue succeeds, with ntdll backoff between attempts.
        // Best-effort paths may choose try_send semantics.
        while self.queue.push(msg).is_err() {
            // Small relative interval in 100ns units (negative = relative).
            // Example: -1000 == 100us.
            let backoff_100ns: i64 = -1_000;
            unsafe { NtDelayExecution(TRUE, &backoff_100ns) };
        }
        if self
            .wake_armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            unsafe { NtAlertThread(self.io_thread); }
        }
    }
}
```

Producers (Session, Surface, read thread) call `io_notify.send(msg)`
instead of `io_tx.try_send(msg)`.

Queue capacity matches prior behavior: 64.

## Platform Abstraction

The ntdll APIs are exposed through a thin platform module, not a
trait. Consumer code (`read_thread.rs`, `io_thread.rs`) imports from
`platform::` — the `#[cfg]` switching happens at the module level.

```
crates/terminal/src/
├── platform/
│   ├── mod.rs          // #[cfg] re-exports
│   ├── windows/
│   │   ├── mod.rs      // pub use ntdll, thread, io
│   │   ├── ntdll.rs    // raw FFI declarations
│   │   ├── thread.rs   // PlatformThread (NtCreateThreadEx)
│   │   └── io.rs       // AsyncRead, AsyncWrite, alertable_wait
│   └── (future: linux/, macos/)
```

### platform::windows::ntdll — Raw FFI

Manual declarations for the ~8 ntdll functions we use, plus local NT
type definitions. We intentionally define our own ABI structs/unions
to mirror Zig's stdlib surface exactly, so we can patch behavior or
layout in one place if Windows/SDK drift appears in the future.

This keeps FFI scope explicit and local to `platform/windows/ntdll.rs`
instead of leaking Windows SDK type choices throughout the crate.

```rust
// Functions:
// NtReadFile, NtWriteFile, NtDelayExecution, NtAlertThread,
// NtCancelIoFileEx, NtCreateThreadEx, NtResumeThread,
// NtWaitForSingleObject, NtClose

// Locally-defined ABI types (Zig-style mirror):
// NTSTATUS, HANDLE, BOOLEAN, ACCESS_MASK
// IO_STATUS_BLOCK (with Status/Pointer union + Information)
// PIO_APC_ROUTINE
// OBJECT_ATTRIBUTES, CLIENT_ID (only fields needed by used syscalls)

// Local NTSTATUS constants: STATUS_SUCCESS, STATUS_PENDING,
// STATUS_ALERTED, STATUS_USER_APC, STATUS_CANCELLED,
// STATUS_PIPE_BROKEN, STATUS_END_OF_FILE, STATUS_TIMEOUT.

// In debug/dev builds we add size+alignment assertions against
// windows-sys equivalents where available, so layout regressions are
// caught immediately.
```

### platform::windows::thread — Thread Lifecycle

```rust
/// A thread spawned via NtCreateThreadEx.
/// Handle has MAXIMUM_ALLOWED access (includes THREAD_ALERT).
pub struct PlatformThread {
    handle: HANDLE,
    id: u32,
}

impl PlatformThread {
    pub fn spawn_suspended(
        entry: unsafe extern "system" fn(*mut c_void) -> u32,
        context: *mut c_void,
    ) -> Result<Self>;

    pub fn resume(&self) -> Result<()>;     // NtResumeThread
    pub fn alert(&self) -> NTSTATUS;        // NtAlertThread
    pub fn handle(&self) -> HANDLE;
    pub fn id(&self) -> u32;
    pub fn join(self);                      // NtWaitForSingleObject + NtClose
}
```

No enum wrapping NTSTATUS returns — callers see the status directly.

### platform::windows::io — IO Primitives

```rust
/// State for one in-flight NtReadFile or NtWriteFile.
pub struct AsyncIo {
    pub iosb: IoStatusBlock,
    pub done: bool,
}

/// Issue NtReadFile with APC. Returns raw NTSTATUS.
pub unsafe fn async_read(
    handle: HANDLE, io: &mut AsyncIo,
    buf: *mut u8, len: u32,
) -> NTSTATUS;

/// Issue NtWriteFile with APC. Returns raw NTSTATUS.
pub unsafe fn async_write(
    handle: HANDLE, io: &mut AsyncIo,
    buf: *const u8, len: u32,
) -> NTSTATUS;

/// Cancel a specific in-flight IO.
pub unsafe fn cancel_io(handle: HANDLE, iosb: &IoStatusBlock) -> NTSTATUS;

/// Alertable wait. Returns raw NTSTATUS.
pub fn alertable_wait(timeout: &i64) -> NTSTATUS;
```

`alertable_wait` returns the raw NTSTATUS (`STATUS_SUCCESS`,
`STATUS_ALERTED`, `STATUS_USER_APC`). No wrapper enum — the caller
matches on the status directly. This keeps the abstraction zero-cost
and avoids encoding Windows-specific concepts like `STATUS_USER_APC`
into a cross-platform type.

### Future Platform Support

When adding Linux/macOS support, the `platform/linux/` module exposes
the same public API shape using `io_uring` or `epoll` + `eventfd`.
The read/IO thread loop structure stays the same — only the
platform calls change. No traits, no vtables, no runtime dispatch.

## Shutdown Sequence

Unchanged in structure from v2. The signaling mechanism changes:

```
 1. UI drops TerminalSession
 2. Session::drop() calls io_notify.send(IoMsg::Close)
      (pushes to queue + NtAlertThread)
 3. IO thread wakes, sees Close
    a. Sets notify.closing = true
    b. NtAlertThread(read_thread_handle)    // wake read thread
    c. Drains remaining messages (flushes pending writes)
    d. Locks hpcon_op → close_async() → unlock
    e. Exits IO loop
 4. Read thread wakes from NtDelayExecution (STATUS_ALERTED)
    a. Sees closing == true → enters drain-to-EOF
    b. Continues issuing NtReadFile until EOF/PIPE_BROKEN
    c. On EOF: cancel remaining, drain APCs (zero-timeout wait)
    d. Emits IoEvent::Exited(status)
    e. Exits read loop
 5. Session::drop() joins read thread (NtWaitForSingleObject)
 6. Session::drop() joins IO thread
 7. PtyWriter drops (Conpty → ClosePseudoConsole join, then conin close)
```

## Dependency Changes

| Dependency | Action | Reason |
|---|---|---|
| `crossbeam-channel` | **Keep** | Still used for `RendererMessage` sender (renderer ↔ IO thread) |
| `crossbeam-queue` | **Add** | `ArrayQueue<IoMsg>` for lock-free bounded queue |
| `windows-sys` features | **Trim to `Win32_Foundation` + `Win32_System_Console`** | ntdll ABI types are local mirrors; keep only `HANDLE` and `COORD` for pty interop |

Manual ntdll FFI (~10 functions) avoids adding `Wdk_Storage_FileSystem`
and its transitive feature tree.

## Risks and Mitigations

**APC alignment.** The low bit of the APC routine pointer must be
clear (the kernel uses it as a flag). Rust function pointers are
naturally aligned to ≥4 bytes on x86_64. No action needed, but
documented here as a known invariant.

**ConPTY pipe mode.** The conout/conin pipes must be opened with
`FILE_FLAG_OVERLAPPED` (async mode) for NtReadFile with APC to work.
Our ConPTY backend already creates them this way. Verified.

**Spurious APC delivery.** `NtDelayExecution` may return
`STATUS_USER_APC` without any of our `done` flags being set (e.g., if
the OS delivers a system APC). The loop handles this naturally —
harvest finds no completed buffers, re-enters the wait.

**NtAlertThread vs NtAlertThreadByThreadId.** These are unrelated
mechanisms despite the similar names. `NtAlertThread(HANDLE)` wakes
alertable waits (`NtDelayExecution`). `NtAlertThreadByThreadId(TID)`
wakes `NtWaitForAlertByThreadId`. We use the former.

**Thread handle lifetime.** `PlatformThread` owns the handle from
`NtCreateThreadEx` through `join()` (which calls `NtClose`). The
handle stored in `ReadThreadNotify`/`IoThreadNotify` is valid for the
entire thread lifetime because `join()` happens after the notify struct
is no longer used.

**Bounded queue backpressure.** `ArrayQueue` is bounded (capacity 64).
This preserves prior mailbox capacity and prevents unbounded growth.
Control/protocol messages (`Close`, `Reply`) must use lossless enqueue
policy (retry with backoff/yield). Best-effort UI messages may use
drop/coalesce policy where explicitly allowed (e.g. resize last-wins).

## File Changes

### New
- `crates/terminal/src/platform/mod.rs`
- `crates/terminal/src/platform/windows/mod.rs`
- `crates/terminal/src/platform/windows/ntdll.rs`
- `crates/terminal/src/platform/windows/thread.rs`
- `crates/terminal/src/platform/windows/io.rs`

### Modified
- `crates/terminal/src/read_thread.rs` — IOCP → APC loop, 4 buffers,
  per-buffer `io.done` flag (shared `flag_apc` from `io.rs`)
- `crates/terminal/src/io_thread.rs` — recv_timeout → queue + NtDelayExecution
- `crates/terminal/src/types.rs` — ReadThreadNotify drops IOCP, adds thread handle;
  add IoThreadNotify with queue + thread handle
- `crates/terminal/src/session.rs` — NtCreateThreadEx, IoThreadNotify, drop PlatformThread
- `crates/terminal/Cargo.toml` — add crossbeam-queue, trim windows-sys features

### Removed
- IOCP creation in session.rs
- `drain_iocp_for_buffers`, `get_overlapped_result_iocp`, `start_read`
  (ReadStart enum) in read_thread.rs
- `CreateEventW`, write OVERLAPPED, `complete_inflight` polling in io_thread.rs
- `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` FFI declaration
- `ReadApcCtx`, `done_mask: AtomicU32`, `seq_slots` — replaced by
  per-buffer `AsyncIo.done` bool + 4-element linear scan
