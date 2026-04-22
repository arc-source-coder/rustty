# In-Process ConPTY with zconpty

> Supersedes the PTY transport architecture described in
> [11-pty-threading-v2.md](11-pty-threading-v2.md) and the transport-era
> APC IO model in [15-ntdll-io-rework.md](15-ntdll-io-rework.md) for the
> end-state Windows design.

## Goal

Embed the console server inside rustty as a library (zconpty). Remove the
external `OpenConsole.exe` process, the `conin` / `conout` transport pipes,
the signal pipe, and the Rust-side PTY read/write thread split. Keep one
real terminal state (Ghostty), one session-owned input subsystem, and one
dedicated console thread for ConDrv dispatch.

## Motivation

The old architecture pays for two boundaries:

1. **Cross-process transport.** rustty talks to `OpenConsole.exe` through
   three pipes (`conin`, `conout`, signal). This is the sole reason for
   separate PTY read / write threads.
2. **Duplicate terminal state.** The external server maintains its own
   parser, state machine, and text buffer, then re-synthesizes VT back
   to the hosting terminal.

These produce the standard ConPTY failure modes: async resize ordering
bugs, double-parsing overhead, second-buffer drift, and transport-only
shutdown complexity (`HPCON`, pipe drain, signal pipe sequencing).

The in-process design collapses both boundaries. Ghostty becomes the
only terminal state.

## Reference Material

- **Windows Terminal in-process spec:**
  `opensrc/repos/microsoft/terminal/doc/specs/#13000 - In-process ConPTY.md`
- **ConDrv message loop:** `opensrc/repos/microsoft/terminal/src/host/srvinit.cpp`
  (`ConsoleIoThread` / `ConsoleCreateIoThread`)
- **ConDrv device comm:** `opensrc/repos/microsoft/terminal/src/server/ConDrvDeviceComm.cpp`
- **InputBuffer:** `opensrc/repos/microsoft/terminal/src/host/inputBuffer.cpp`
- **VT input → input buffer:**
  `opensrc/repos/microsoft/terminal/src/terminal/adapter/InteractDispatch.cpp`
- **Ghostty terminal internals:**
  `zig/ghostty/src/terminal/Terminal.zig`,
  `zig/ghostty/src/terminal/Screen.zig`

## Implementation Notes

### Prefer ntdll on hot paths

Use `NtDeviceIoControlFile` for `ReadIo` / `CompleteIo`, `NtOpenFile`
for ConDrv handle creation. This avoids kernel32 translation layers and
aligns with Zig's cleanup work
([PR 31126](https://codeberg.org/ziglang/zig/pulls/31126),
[PR 31136](https://codeberg.org/ziglang/zig/pulls/31136)). Cold setup
paths may still use Win32 APIs where they are clearer.

### simdutf at encoding boundaries

Minimize the number of UTF encoding boundaries, then make the remaining
ones fast with `simdutf`. Primary targets: UTF-16 console API text
entering Ghostty's UTF-8 paths. Do not introduce conversion in paths
that stay structured (keyboard/mouse/focus handling).

## Non-Goals (v1)

- Arbitrary offscreen screen-buffer geometry or arbitrary N screen buffers
- A second console-only text buffer mirroring Ghostty
- Using `RenderState` as the authoritative server state
- Show/hide / set-parent signal-pipe semantics
- User scrollback position affecting Win32 console API readback

## Architecture with OpenConsole.exe

Today rustty spawns the external console server and wraps it with
transport threads. This is 8 threads for a single console session.

```
┌──────────────────────────────────────────────────────────┐
│ rustty process                                           │
│                                                          │
│  ┌────────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ GPUI main      │  │ renderer     │  │ pty-io       │  │
│  │                │  │              │  │              │  │
│  │ user input ────┼──┼──────────────┼─►│ conin pipe ──┼──┼──►┐
│  │                │  │ terminal     │  │              │  │   │
│  │ window/comp ───┼─►│ mutex ◄──────┼──┼── pty-read ◄─┼──┼───┼─┐
│  │ events         │  │ render_frame │  │ feed()       │  │   │ │
│  └────────────────┘  └──────────────┘  └──────────────┘  │   │ │
│                                                          │   │ │
│  Ghostty terminal (single state)                         │   │ │
└──────────────────────────────────────────────────────────┘   │ │
                                                               │ │
┌──────────────────────────────────────────────────────────┐   │ │
│ OpenConsole.exe / conhost.exe                            │   │ │
│                                                          │   │ │
│  ConsoleIoThread ◄── ConDrv                              │   │ │
│  VtInputThread   ◄── conin pipe ◄────────────────────────┼───┘ │
│  PtySignalInputThread ◄── signal pipe                    │     │
│  Win32 pseudo-window thread                              │     │
│                      conout pipe ────────────────────────►┼─────┘
└──────────────────────────────────────────────────────────┘
```

In headless ConPTY mode the external server still owns four threads
(`ConsoleIoThread`, `VtInputThread`, `PtySignalInputThread`, Win32
pseudo-window), on top of rustty's own renderer, PTY read, and PTY IO
threads. This is 8 threads for a single console session.

## Architecture with zconpty

zconpty is an in-process console server library. The host provides a
terminal vtable; zconpty owns all Win32 console semantics, ConDrv
dispatch, input routing, and cooked-read editing.

```
┌──────────────────────────────────────────────────────────────────────┐
│ rustty process                                                       │
│                                                                      │
│  ┌────────────────┐                                                  │
│  │ GPUI main      │                                                  │
│  │                │                                                  │
│  │ key/mouse/ ────┼──► IoMsg::Input(...) ───────►┐                   │
│  │ focus/paste    │                              │                   │
│  │                │                              ▼                   │
│  │ window/comp    │                    ┌─────────────────────┐       │
│  │ events      ───┼─►────────────────► │ IO thread (Rust)    │       │
│  └────────────────┘                    │                     │       │
│                                        │ timers              │       │
│  ┌────────────────┐                    │ resize coalesce     │       │
│  │ renderer       │                    │ zconpty_send_*()    │       │
│  │                │                    └──────────┬──────────┘       │
│  │ render_update  │                               │                  │
│  └────────────────┘                               │                  │
│                                                   ▼                  │
│               ┌──────────────────────────────────────────────┐       │
│               │ console thread (Zig, zconpty)                │       │
│               │                                              │       │
│               │ ReadIo ◄── ConDrv                            │       │
│               │ vtable.feed ── terminal mutation             │       │
│               │ ReadConsole / GetConsoleInput setup          │       │
│               │ writeInput() for device responses            │       │
│               └──────────────────────────────────────────────┘       │
│                                                                      │
│  Ghostty terminal (single state, accessed via vtable)                │
└──────────────────────────────────────────────────────────────────────┘
```

## Core Invariants

### 1. One session owns one server state bundle

Each terminal session owns exactly one: ConDrv server handle, console
thread, input subsystem, Ghostty terminal instance, and
main/alternate screen pair.

### 2. One real terminal state

Ghostty is the terminal state. There is no second VT parser, mirrored
text buffer, or mirrored state machine.

### 3. One terminal mutex

A single terminal mutex is shared between the console thread (terminal
mutation via `.feed()`) and the renderer thread (`render_frame()` →
`render_update()`).

## Library Boundary

zconpty is a library. The host provides a terminal vtable; zconpty
provides all Win32 console server semantics. This boundary is the
architectural center of the design.

**The host provides:**

- Terminal `feed()` for output mutation
- VT input encoding (`vt_encode_key`, `vt_encode_mouse`, etc.)
- Terminal queries (size, cursor, cell data, palette)

**zconpty provides:**

- ConDrv `ReadIo` / `CompleteIo` loop (console thread)
- Input routing: VT fast path, legacy `INPUT_RECORD` path, cooked read
- Waiter / pending-read management
- Typeahead storage
- Process management and control events
- Cooked `ReadConsole` line editing, history, and redraw
- All `WriteOutput + CompleteIo` work

The vtable is defined in `vendor/zconpty/src/server/Terminal.zig`.

## Input Model

zconpty owns input semantics. The host sends normalized input events
via `zconpty_send_*()` (key, mouse, paste, focus, resize). zconpty
decides how to route them.

### Three read paths

`ReadConsole` dispatch:

1. If `ENABLE_LINE_INPUT` is set → **cooked line-input path**
2. Else if `ENABLE_VIRTUAL_TERMINAL_INPUT` is set → **VT fast path**
3. Else → **raw text path** (legacy `InputBuffer`)

Legacy event reads (`GetConsoleInput` / `ReadConsoleInput`) are a
separate fourth path using `INPUT_RECORD` structs.

### Routing order

When input arrives, zconpty checks in this order:

1. Active cooked read → advance the line editor
2. Pending VT read slot → fill buffer, `CompleteIo` inline
3. Pending legacy waiter → fill `INPUT_RECORD`s, `CompleteIo`
4. Fallback to current mode flags → buffer into VT overflow or
   `InputBuffer` storage

### Device responses

Device responses (DA, DSR, CPR) generated by `feed()` are injected
directly into the input path on the console thread. They do not go
through the host input queue.

## Cooked Read

zconpty owns cooked `ReadConsole` with `ENABLE_LINE_INPUT` — the path
`cmd.exe` uses by default.

- One active cooked read per session
- UTF-8 internal line buffer with `uucode`-backed grapheme/cell editing
- Console-owned per-exe history with bounded pool and optional dedup
- `CtrlWakeupMask` and `InitialNumBytes` for `cmd.exe` tab-completion
- VT-based terminal redraw through `feed()` (no second text buffer)
- Authoritative redraw after interleaved output

## Readback Model

Server reads query Ghostty directly through the terminal vtable using
`get_cursor_position`, `get_size`, etc.

- Console buffer size == viewport size
- APIs requiring larger arbitrary backing buffers fail with `E_NOTIMPL`
- User scrollback position never affects console API readback

## Synchronization Summary

```
┌──────────────────┬──────────────────┬───────────────────────────────┐
│ Resource         │ Synchronization  │ Contention                    │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ Terminal state   │ Mutex (in shim)  │ Console thread (feed)         │
│ (Ghostty)        │                  │ vs IO thread (encode)         │
│                  │                  │ vs Renderer (render_update)   │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ VtInputSlot      │ Mutex            │ IO thread vs console thread   │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ InputBuffer      │ Mutex            │ IO thread vs console thread   │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ CookedReadSlot   │ Mutex            │ IO thread vs console thread   │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ input_mode       │ AtomicU32        │ Console stores, IO loads      │
├──────────────────┼──────────────────┼───────────────────────────────┤
│ Rust IO queue    │ ArrayQueue       │ GPUI produces, IO drains      │
└──────────────────┴──────────────────┴───────────────────────────────┘
```

**Lock ordering: terminal lock → input slot/buffer lock. Never reverse.**

## What Disappears

Removed entirely:

- Named-pipe `conin` / `conout` transport
- Signal pipe and signal thread
- Rust PTY read thread
- Rust PTY writer/timer thread as a transport writer
- `HPCON` close thread and pipe-drain shutdown path
- Duplicate output parser / duplicate text buffer
- `INPUT_RECORD` synthesis on the VT input hot path

## Compatibility Policy

Initial target: `cmd.exe`, Windows PowerShell 5, representative TUIs.

- Cooked-read support is implemented (`cmd.exe` relies on it)
- Win32 attributes without a useful VT equivalent may be dropped
- Screen-buffer requests outside the viewport-sized model fail cleanly
  with `E_NOTIMPL`

## Open Questions

1. Whether `ReadIo` ships async on day 1 or after a focused proving pass
2. `GetConsoleWindow` / pseudo-window identity strategy once the
   external server process is removed
3. Cooked-read echo path re-entrancy during device responses
4. Exact `VtInputSlot` overflow capacity policy

## Summary

The end-state Windows architecture:

- **One library** (zconpty) replacing `OpenConsole.exe`
- **One terminal state**: Ghostty, accessed via vtable
- **Three text-read paths**: cooked, VT fast, raw — plus legacy event reads
- **One terminal mutex** shared between console mutation and renderer
- **No transport pipes, no signal pipe, no external console server**
