# Roadmap and Delivery Plan

## Development Principles

- Build vertical slices first.
- Keep each crate API minimal and explicit.
- Prefer measurable wins over speculative complexity.

## Phase 0: Foundation

Deliverables:

- Workspace crates scaffolded with clear dependencies
- `ghostty_vt` crate: Zig shim + FFI + safe Rust wrapper with smoke tests
- Zig `0.15.2` version guard in build script
- `pty` crate spawning `powershell.exe`

Acceptance:

- Builds on Windows with system Zig 0.15.2
- Can spawn shell and exchange bytes in a test harness
- `ghostty_vt::Terminal` can feed bytes and read back rows

## Phase 1: First Vertical Slice

Deliverables:

- One terminal tab end-to-end
- Keyboard input to PTY
- PTY output rendered via dirty-row path
- Resize handling

Acceptance:

- Interactive shell usage works reliably
- No hard UI stalls during moderate output

## Phase 2: Tabs and App Shell

Deliverables:

- Custom titlebar with tab strip (Windows Terminal style)
- Add/close/switch tab actions
- App closes on last tab close
- Tab title updates from terminal OSC sequences

Acceptance:

- Tab lifecycle is deterministic
- Active tab state and focus behavior are correct

## Phase 3: v0 Quality Features

Deliverables:

- Selection, copy/paste basics
- Kitty keyboard protocol mode support
- Improved dirty/caching behavior under heavy output
- Baseline regression suite for VT + PTY

Acceptance:

- Stable behavior under long command output
- Key protocol behavior validated by tests

## Test Plan

### VT Golden Tests

- Style and color sequence handling
- Cursor movement and mode transitions
- Dirty-row extraction behavior

### PTY Integration Tests

- Process spawn/write/read/exit
- Resize correctness
- Command ordering guarantees

### Renderer Smoke Tests

- Repaint after dirty updates
- Selection and cursor overlays
- Tab switch + focus behavior

## Alternatives and Why Not Chosen

1. Build all UX features first, optimize later
   - rejected: terminal performance work is foundational, not optional

2. Skip tabs until after full terminal maturity
   - rejected: tabs are a core UX requirement; we include them in constrained form

3. Build custom GPU text engine now
   - rejected: too much complexity for early value; already handled by GPUI

## Risks and Planned Mitigations

- Zig compatibility drift
  - enforce exact version check and clear build diagnostics
- Ghostty internal API changes across versions
  - Zig shim isolates breakage to single crate; update shim on version bump
- heavy-output hitches
  - budgeted UI drain loop + run cache + dirty rows
- resize race/visual jitter
  - coalesced resize and synchronized update path
- architecture drift as features grow
  - keep crate boundaries strict, avoid convenience leaks

## Future Optimization Targets

- Replace scalar UTF-8 decoder in Zig shim with SIMD (highway or custom)
- Expand shim query/response coverage (XTVERSION, XTGETTCAP, advanced queries)
- GPU glyph atlas for highest-throughput text rendering
