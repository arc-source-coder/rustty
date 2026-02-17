---
title: Terminal session core (TerminalSession entity, drain loop, side effects, config models)
status: open
priority: 1
created-at: "2026-02-23T03:13:48Z"
blockers:
  - ffi-001
  - pty-001
---

TerminalSession as GPUI Entity: owns ghostty_vt::Terminal + PTY channel handles (pty_tx: Sender<PtyCommand>, pty_rx: Receiver<PtyEvent>). Budgeted drain loop: 2ms wall-clock per tick, try_recv in loop, always finish a complete Output chunk. Side-effect queue: Vec<SideEffect> (TitleChanged/Bell/ClipboardWrite/ClipboardRead) — callbacks push, processed after drain completes (prevents reentrancy). Device response forwarding: read ResponseBuffer after feed() → PtyCommand::Write before user input. SessionMetadata (title, cwd, bell_count, has_unread_output). ProcessState (Running/Exited/Error). SpawnConfig (immutable after creation) + RenderConfig (shared Model, hot-reloadable). Wire cx.notify() for repaint. Synchronized output: defer cx.notify() while mode active, 1s safety timer to force-clear. Reschedule drain if channel still has data. Refs: docs/architecture/02-data-model.md § Terminal Session Model + § Device Responses, docs/architecture/04-pty-threading.md § Budgeted Drain Policy + § Write Ordering Rules, docs/architecture/03-rendering.md § Synchronized Output.
