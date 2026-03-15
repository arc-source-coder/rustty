# Improvements Tracker

Purpose: short, implementation-facing parity/improvement items we want to track across workstreams.

## Selection Parity

- **URL-aware double-click selection:** On double-click, attempt link detection before word selection (Ghostty uses `linkAtPin` before `selectWord`).
  - ref: `crates/ghostty-vt/zig/ghostty/src/Surface.zig` (double-click branch around line ~4113, helper `linkAtPin` around line ~4431).
- **Pixel-threshold drag selection:** Include/exclude endpoint cells using horizontal threshold logic instead of pure cell anchoring (Ghostty uses `mouseSelection` threshold rules).
  - ref: `crates/ghostty-vt/zig/ghostty/src/Surface.zig` (`mouseSelection` around line ~4901, threshold notes around line ~4915).
