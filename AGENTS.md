Kairo is a fast, GPU-accelerated terminal emulator for Windows using GPUI + Ghostty.

- The UI is inspired by Windows Terminal and Windows Fluent Design, as well as Ghostty.
- The terminal is rendered by a D3D11 Rust renderer (crates/renderer/) built on Ghostty's RenderState API.
- The font pipeline is built on Harfbuzz, DirectWrite, and Direct2D (crates/font/).
- Kairo does not use Windows ConPTY. It uses `zconpty` (vendored at `vendor/zconpty/`).

### Notes

- `opensrc/` is gitignored - Grep/Glob will not find contents of repos/packages inside the directory by defa. When using searching inside opensrc/, explicitly set the directory parameter of the Grep tool to opensrc/.

### Reference

1. `vendor/zed/crates/gpui` - Source code for GPUI. Also see `vendor/zed/crates/gpui_*` when needed.
2. `zig/ghostty/` - Ghostty source code. Inspiration for UI / feature design, architecture, and data models. Vendored as a submodule. The Rust wrapper around the Zig shim is located at `crates/ghostty/`. The shim itself is located at `zig/` to wrap the vendored Ghostty source.
3. `opensrc/repos/microsoft/terminal` - Windows Terminal and OpenConsole source code. Refer when working with Windows-specific code such as Windows PTY semantics or D3D11/DirectWrite-specific code.
4. `vendor/zed/crates/ui` - General Zed UI components for reference.
5. `opensrc/packages/microsoft/windows-rs` - `windows` / `windows-sys` crate source code.
6. `opensrc/repos/microsoft/microsoft-ui-xaml` - WinUI 3 source code.
7. `vendor/harfbuzz/` - HarfBuzz source code vendored as a submodule.
8. `vendor/zed/crates/ui` - General Zed UI components for reference.
