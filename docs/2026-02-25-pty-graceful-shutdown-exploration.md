# PTY Shutdown Improvement Exploration: Graceful ConPTY Close

**Date:** 2026-02-25  
**Status:** Proposed (not implemented)  
**Related:** `crates/pty`, `crates/pty/src/windows/conpty.rs`

## Problem

Current Windows PTY shutdown uses immediate `TerminateProcess` to force child exit, then waits for exit event, then drops ConPTY (which closes HPCON). This is coarse — apps don't get console close signals.

Windows Terminal (WT) does graceful HPCON close first, which sends `CTRL_CLOSE_EVENT` to clients, then waits for graceful exit, with a timeout fallback to force kill.

## References

- Windows Terminal `ConptyConnection::Close()` — `opensrc/repos/microsoft/terminal/src/cascadia/TerminalConnection/ConptyConnection.cpp:662-698`
- ConPTY CTRL_CLOSE_EVENT semantics
- Current fix in `crates/pty/src/handle.rs`, `crates/pty/src/windows/child.rs`, `crates/pty/src/windows/mod.rs`
