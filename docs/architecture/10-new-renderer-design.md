# Reworked Renderer Design

## Problem

The [previous rendering architecture document] (docs/architecture/03-rendering.md) has an important flaw: It says: '`terminal` calls `cx.notify()` when repaint is needed' - however, TerminalSession is **one Entity**. All rows are part of this entity. Calling cx.notify on TerminalSession means the entire terminal we be re-rendered, nullifying the benefit of the dirty-row optimization entirely.

## The Solution

Build a *Custom Element* - `TerminalElement` which implements the `Element` trait. The `render` method (required by the `Render` trait) on `TerminalView` (in renderer crate - a wrapper around TerminalSession - it simply holds a handle to TerminalSession.) will return `TerminalElement::new(cx.entity())`, which is a handle to the Entity. 

GPUI calls the render function each time we call cx.notify(). However, we implement the optimization on `TerminalElement` - specifically in the `prepaint` method (Ref: check the Element trait in vendor/zed/crates/gpui/src/element.rs). 

The flow looks something like: call `render_update` (We call it here because it allows getting state at the latest possible moment) -> call `begin_frame` -> match on dirty state -> only rebuild row caches for dirty rows (pseudocode: if row y is dirty, row_runs[y] = build_runs(y.cells) -> update our cache).

We store per-row BatchedTextRun (could be named better - idea is Vec<Vec<BatchedTextRun>>) cache, cursor state, and other derived data on TerminalElement's element state (the PrepaintState) as a LayoutState (also could be named better). The LayoutState is what we update. We then return the updated LayoutState -> paint() uses it to render. Related: See .with_element_state() from GPUI.

