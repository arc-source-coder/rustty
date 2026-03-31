Rustty is a fast, clean, GPU-accelerated terminal emulator for Windows using GPUI + ghostty. UI is inspired by Windows Terminal. Titlebar tabs + (later) Windows Fluent Design styled context/dropdown menus. Terminal rendering is done by a GPU-accelerated render (Rust) built on Ghostty's RenderState API. The text pipeline is built on DirectWrite (crates/font/). 

### Notes

- The opensrc/ directory is gitignored. Because of this, grep/glob will not find contents of repos/packages inside opensrc. When using searching repos/packages inside opensrc/, explicitly set the directory parameter of the Grep tool to opensrc/.
- Whenever something from the docs looks unclear / seems off, look at (or spawn finder agents to look at) what Ghostty does (`zig/ghostty/`), so we can either directly do what they do or replicate the semantics. Please stop and mention these to the user whenever you encounter them.

### Reference

1. `vendor/zed/crates/gpui` - Source code for GPUI.
2. `zig/ghostty/` - The Ghostty source code (latest - version 1.3 using Zig 0.15.2). Inspiration for UI / feature design, architecture, and data models. Vendored as a submodule. The Rust wrapper around the zig shim is located at `crates/ghostty/`.
3. `opensrc/repos/MitchForest/rust-terminal` - Implementation of a terminal emulator using GPUI - inspired by Ghostty's UI style. Code organization is excellent.
4. `opensrc/repos/microsoft/terminal` - Windows Terminal source code. Refer when working with Windows-specific code such as the Windows PTY backend / DirectWrite-specific code.
5. `vendor/zed/crates/terminal` - Core business logic for Zed's terminal - based off the Alacritty crate.
6. `vendor/zed/crates/terminal_view` - UI layer / Rendering through GPUI for the Alacritty-based terminal.
7. `vendor/zed/crates/ui` - General Zed UI components for reference.
8. `docs/architecture` - architecture information. Refer as needed.
9. `docs/reference` - mockup UI images
10. `opensrc/packages/microsoft/windows-rs` - `windows` / `windows-sys` crate source code.
