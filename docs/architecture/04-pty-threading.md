# PTY and Threading Design

## Objective

Keep IO robust and simple while protecting UI responsiveness.

## v0 Backend

- `portable-pty` as initial backend (cross-platform: ConPTY on Windows, Unix PTY elsewhere)
- Default shell: `powershell.exe`

## Why We Do Not Reuse Ghostty `termio`

Ghostty's `termio` implementation is tightly coupled to Ghostty runtime
primitives (`xev` event loop, termio/surface mailboxes, renderer mutex
protocol). We cannot embed it directly without pulling in Ghostty's runtime.

Instead, we mirror the same invariants in Rust:

- single write path back to PTY
- bounded queues with backpressure
- explicit write ordering for device responses vs user input
- strict shutdown order (signal stop, unblock read, join threads)

## Thread Ownership

UI thread:

- Owns `ghostty_vt::Terminal`
- Owns terminal session orchestrator state

PTY thread:

- Owns PTY handles and process lifecycle
- Performs blocking reads/writes/resizes

Invariant: PTY thread never touches emulation data structures.

## Channels

### PTY -> UI

- Bounded queue of `PtyEvent`
- Payload mostly `Output(Vec<u8>)`
- May block PTY read thread when full (backpressure)

### UI -> PTY

- Queue of `PtyCommand`
- `Write` preserves ordering
- `Resize` coalesced last-wins

## Write Ordering Rules

Multiple sources produce `PtyCommand::Write` bytes:

1. **User input** — keystrokes, paste, mouse reports encoded by the
   `terminal` crate
2. **Device responses** — DA, DSR, kitty keyboard query responses
   produced by the shim's `ResponseBuffer` during `feed()`
3. **Resize** — coalesced separately (not byte-ordering sensitive)

Ordering invariants:

- **Device responses are written first**, immediately after the drain
  cycle that triggered them, before any subsequent user input writes.
  This matches Ghostty's behavior where `StreamHandler` writes responses
  back to the PTY inline during parsing.
- **User input writes preserve insertion order.** Multiple keystrokes
  within one UI tick are concatenated and sent as a single write.
- **All writes go through the single `pty_tx` channel.** There is no
  second write path. This prevents interleaving.

## Device Response Scope (Phased)

Generated in the shim handler during parse (`ResponseBuffer`), then written
through normal PTY write channel:

- v0 required: DA, DSR, DECRPM (`request_mode`), kitty keyboard query,
  size report, ENQ
- later: XTVERSION, XTGETTCAP, additional optional queries

This mirrors Ghostty's model where responses are generated in stream
handling and sent through IO messaging, not ad-hoc from renderer/UI code.

## Mouse Encoding Boundary

Mouse reporting behavior depends on terminal mode flags and report format.
To keep behavior correct:

- UI layer sends semantic mouse events (button/motion/wheel + modifiers +
  coordinates)
- terminal/shim encoding layer serializes escape bytes based on current
  mode/format flags
- renderer/UI does not hand-roll protocol bytes

Concrete drain cycle write order:

```text
1. Drain PtyEvent::Output bytes from channel
2. Feed bytes to ghostty_vt::Terminal (handler fires callbacks,
   ResponseBuffer collects device response bytes)
3. Read ResponseBuffer → send as PtyCommand::Write (high priority)
4. Process any queued user input → send as PtyCommand::Write
5. Call render_update() + cx.notify()
```

### Reentrancy Prevention

Shim callbacks (title, bell, clipboard) must not call back into
`TerminalSession` methods that can reenter `feed()`. Instead:

- Callbacks push events into a session-local `Vec<SideEffect>` queue.
- The queue is processed after the drain loop completes.
- This prevents recursive `feed()` calls and keeps the drain cycle
  atomic.

```rust
enum SideEffect {
    TitleChanged(String),
    Bell,
    ClipboardWrite(Vec<u8>),
    ClipboardRead,
}
```

## Budgeted Drain Policy

### Budget by Time, Not Bytes

The drain loop is budgeted by **wall-clock time on the UI thread**
(target: 2ms per tick), not by byte count or message count. Byte sizes
are too variable — a single `PtyEvent::Output` can be 64 bytes or 64KB.

```rust
const DRAIN_BUDGET: Duration = Duration::from_millis(2);

fn drain_pty_output(&mut self, cx: &mut Context) {
    let deadline = Instant::now() + DRAIN_BUDGET;

    while Instant::now() < deadline {
        match self.pty_rx.try_recv() {
            Ok(PtyEvent::Output(bytes)) => {
                self.terminal.feed(&bytes);
            }
            Ok(PtyEvent::Exited(status)) => {
                self.process_state = ProcessState::Exited(status);
                break;
            }
            Ok(PtyEvent::Error(e)) => {
                self.process_state = ProcessState::Error(e);
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    // Flush device responses before user input
    self.flush_response_buffer();
    // Update render state + schedule repaint
    self.terminal.render_update();
    cx.notify();

    // If channel still has data, schedule another wake
    if !self.pty_rx.is_empty() {
        cx.spawn(|this, mut cx| async move {
            this.update(&mut cx, |this, cx| this.drain_pty_output(cx))
        }).detach();
    }
}
```

### Complete Buffer Rule

Always finish processing a complete `PtyEvent::Output` chunk once
started. Do not split a buffer mid-feed — Ghostty's stream parser
handles incremental input correctly, but splitting at arbitrary byte
boundaries is unnecessary complexity.

### Synchronized Output Mode (DEC 2026)

When `terminal.modes.get(.synchronized_output)` is true, the drain loop
still processes bytes (the terminal state must stay current), but
**`cx.notify()` is deferred** — no repaint is scheduled until
synchronized output mode is cleared.

This matches Ghostty's approach: its renderer checks
`terminal.modes.get(.synchronized_output)` and returns early (skipping
both `RenderState.update()` and painting) while the mode is active. The
mode is cleared when the program sends `CSI ? 2026 l`.

Safety: if synchronized output stays enabled for >1 second (program
crashed or misbehaving), force-clear the mode and repaint. Ghostty uses
the same 1-second timeout via a safety timer in its termio thread.

## Resize Policy

- Renderer computes new grid size from bounds and font metrics
- Sends resize command to PTY thread
- Applies terminal resize on UI side in same logical update phase
- Avoid resize storms by coalescing high-frequency updates
- Resize force-clears synchronized output mode (per spec, matching
  Ghostty's behavior)

## Process Lifecycle

- Tab close sends `PtyCommand::Close`
- PTY thread terminates process/session resources
- Emits `Exited` event to UI
- Window closes itself when its last tab is removed

Shutdown ordering invariant:

1. Signal PTY/process stop
2. Unblock any blocking read (platform-specific interrupt)
3. Drain queued terminal output/events
4. Emit final `Exited`/`Error` state
5. Join PTY/read worker threads

## Error Handling

- PTY errors become structured terminal session events
- Surface user-visible status in tab/session state via `ProcessState`
- Avoid silently discarding IO failures

## ConPTY / Windows Testing Requirements

`portable-pty` abstracts most platform differences, but ConPTY has known
edge cases that require explicit testing:

- **UTF-16 chunking**: ConPTY may split multi-byte sequences at chunk
  boundaries. Verify that the stream parser handles partial sequences
  across consecutive `PtyEvent::Output` buffers.
- **Resize during heavy output**: ConPTY resize can race with pending
  output. Test that grid coherence is maintained when resizing during
  sustained output (e.g., `cargo build`).
- **Fast close sequences**: Rapid shell exit (exit code + EOF) may arrive
  in the same or immediately consecutive reads. Verify that `Exited`
  state is reached cleanly without hanging.
- **Process exit detection**: ConPTY may not signal EOF the same way Unix
  PTYs do. Test that `PtyEvent::Exited` is reliably emitted.

## Alternatives

1. Separate terminal worker thread for emulation
   - stronger isolation, but higher complexity and Send/Sync pressure
   - rejected for v0

2. Async runtime-first IO model
   - good long-term option if needed
   - unnecessary complexity for initial stable core
