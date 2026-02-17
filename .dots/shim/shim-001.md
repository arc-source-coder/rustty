---
title: Zig shim build system + core shim (TerminalHandle, ShimHandler, lifecycle/feed/resize)
status: open
priority: 1
created-at: "2026-02-23T03:13:02Z"
---

Set up build.rs for ghostty_vt crate: compile Zig shim against vendored Ghostty 1.3.x, Zig 0.15.2 version guard. Write core Zig shim: TerminalHandle struct (alloc, terminal, stream, handler, render_state), ShimHandler with ReadonlyHandler delegation, scalar UTF-8 fallback. Export C ABI: ghostty_vt_terminal_new/free/set_callbacks/feed/resize. Refs: docs/architecture/06-ghostty-shim.md (full shim design + ABI surface + handler architecture), docs/architecture/01-system-overview.md § Ghostty Integration Strategy, opensrc/repos/Xuanwo/gpui-ghostty (1.2.x reference — adapt for 1.3.x), vendor/ghostty/src/terminal/ (internal APIs).
