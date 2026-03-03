Rustty is a fast, clean, GPU-accelerated terminal emulator for Windows using GPUI + ghostty. UI is inspired by Windows Terminal. Titlebar tabs + (later) Windows Fluent Design styled context/dropdown menus.

### Notes

- The opensrc/ directory is gitignored. Due to this, grep/glob might not find contents of repos/packages inside opensrc. When using Grep inside opensrc/, explicitly set the directory parameter.
- Whenever something from the docs/ looks unclear / seems off, look at (or spawn finder agents to look at) what Ghostty does (`crates/ghostty-vt/zig/ghostty`), so we can either directly do what they do or replicate the semantics. Please stop and mention these to the user whenever you encounter them.
- Whenever you come across a piece of code that's related to anything from the "future optimizations" or similar section, inform the user what the area is related to + what the optimization is.

### Reference

1. `vendor/zed/crates/gpui` - Source code for GPUI.
2. `crates/ghostty-vt/zig/ghostty` - The ghostty source code (latest - version 1.3 using Zig 0.15.2). Inspiration for UI + feature design. Vendored as a submodule.
3. `opensrc/repos/Xuanwo/gpui-ghostty` - An implementation of ghostty + gpui that allows embedding it in GPUI applications. Could be very helpful as reference, but we'd want to ensure that we keep our version as clean as possible. Also note: `gpui-ghostty` is vendoring Ghostty 1.2.x - which uses Zig 0.14 + is old. We want to use the latest Ghostty 1.3.x series, which has had multiple performance optimizations + better API + uses the latest Zig 0.15.2).
4. `opensrc/repos/MitchForest/rust-terminal` - Implementation of terminal emulator using GPUI - inspired by Ghostty's UI style. Code organization is excellent.
5. `opensrc/repos/microsoft/terminal` - Windows Terminal source code. Refer when working with Windows-specific code such as the Windows PTY backend. 
6. `vendor/zed/crates/terminal` - Core business logic for Zed's terminal - based off the Alacritty crate.
7. `vendor/zed/crates/terminal_view` - UI layer / Rendering through GPUI for the Alacritty-based terminal.
8. `vendor/zed/crates/ui` - General Zed UI components for reference.
9. `docs/architecture` - architecture information. Refer as needed.
10. `docs/specs` - spec for the app
11. `docs/reference` - mockup UI images
