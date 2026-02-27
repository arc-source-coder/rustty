---
title: Shaped run cache
status: open
priority: 2
created-at: "2026-03-01T06:26:46Z"
---

Shaped run cache (Level 2 content-hash cache). Content-hash keyed cache for pre-shaped text cells. Hash from: utf8 text + font family/weight/style using rapidhash. Enables identical text at different columns to share cache entries. Reduces redundant text shaping under sustained output. Ghostty ref: src/font/shaper/Cache.zig (CacheTable, 256 capacity). Refs: docs/architecture/03-rendering.md (Two-Level Caching, lines 46-69), renderer-001 plan (Part 5 notes as future optimization).
