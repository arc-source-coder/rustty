---
title: Add cursor blinking support
status: open
priority: 2
created-at: "2026-03-05T07:48:54Z"
---

Implement cursor blinking based on CursorState.blinking flag. Ghostty provides blinking state via CursorState::blinking. Need to integrate with GPUI's animation/timer system to toggle cursor visibility.
