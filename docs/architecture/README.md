# Architecture Docs Index

This directory contains implementation-facing architecture documentation for the Windows GPUI + Ghostty terminal project.

- `01-system-overview.md`: system boundaries, crates, ownership, data flow, Ghostty integration strategy
- `02-data-model.md`: core Rust models, messages, and state ownership
- `03-rendering.md`: rendering pipeline, dirty updates, caching strategy
- `04-pty-threading.md`: PTY abstraction, threading, backpressure, lifecycle
- `05-roadmap.md`: staged delivery plan, acceptance criteria, alternatives
- `06-ghostty-shim.md`: Ghostty API landscape, Zig shim design, RenderState, key encoding, mode flags
- `07-future-design.md`: split panes, search, DnD, images, config reload, multi-window — how Ghostty does each, what we've prepared now
- `08-ghostty-alignment-decisions.md`: persisted design decisions from Ghostty source verification (viewport, coordinates, dirty model, FFI lifetime, termio strategy)
- `09-zig-build-investigation.md`: Zig/Cargo integration investigation and build-system decisions
- `10-new-renderer-design.md`: renderer redesign and draw-model direction
- `11-pty-threading-v2.md`: previous external-ConPTY threading model (superseded for the end-state Windows design)
- `12-zero-copy-ffi-rework.md`: zero-copy RenderState FFI contract and detached frame model
- `13-gpu-renderer.md`: GPU renderer architecture and implementation notes
- `14-improvements-tracker.md`: concise backlog of Ghostty parity and architecture improvement items
- `15-ntdll-io-rework.md`: transport-era APC IO design for external ConPTY (superseded for the end-state Windows design)
- `16-in-process-conpty.md`: in-process ConPTY server architecture, waiter model, and three-thread session design

Design goals across all docs:

- simplest design that remains performant under real terminal workloads
- explicit ownership boundaries
- minimal hidden coupling
- easy onboarding and maintenance
