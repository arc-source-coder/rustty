---
title: Input encoding (TerminalInput enum, GPUI event normalization, key/mouse/paste encoding)
status: open
priority: 2
created-at: "2026-02-23T03:13:49Z"
blockers:
  - terminal-001
---

TerminalInput enum: Text(String), Key(NormalizedKeyEvent), Paste(String), Mouse(NormalizedMouseEvent), FocusChanged(bool). Renderer normalizes GPUI KeyDownEvent/TextInput/MouseDown etc. into TerminalInput. Terminal session encodes TerminalInput → VT bytes: key encoding via shim encode_key (Options.fromTerminal() handles all protocol selection internally — kitty keyboard, DEC cursor keys, keypad, modifyOtherKeys). Mouse encoding via shim encode_mouse (mode/format flags read internally). Paste: check is_bracketed_paste mode flag, wrap with \e[200~ / \e[201~ if active. Focus: send \e[I / \e[O if focus reporting mode enabled. All encoded bytes → PtyCommand::Write. Refs: docs/architecture/06-ghostty-shim.md § Key Encoding + § Mouse Encoding, docs/architecture/02-data-model.md § Input Model, docs/architecture/04-pty-threading.md § Mouse Encoding Boundary.
