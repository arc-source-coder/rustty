---
title: Device responses (DA, DSR, DECRPM, kitty query, size report, ENQ)
status: open
priority: 2
created-at: "2026-02-23T03:14:39Z"
blockers:
  - terminal-001
---

Implement device response generation in the ShimHandler's ResponseBuffer. v0 required responses: device_attributes (DA1/DA2 — report VT capabilities), device_status (DSR — cursor position report, operating status), request_mode (DECRPM — report DEC private mode state), kitty_keyboard_query (report current kitty keyboard flags), size_report (report terminal dimensions), enquiry (ENQ — configurable response string). ShimHandler.vt() intercepts these actions, writes response bytes to ResponseBuffer. After feed() completes, Rust side reads ResponseBuffer → sends as PtyCommand::Write (high priority, before user input writes). Test with programs that probe capabilities: neofetch, htop, fish shell, vim. Refs: docs/architecture/02-data-model.md § Device Responses, docs/architecture/04-pty-threading.md § Device Response Scope + § Write Ordering Rules, docs/architecture/06-ghostty-shim.md § ShimHandler.
