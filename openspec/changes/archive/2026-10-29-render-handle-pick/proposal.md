## Why

A host whose render delegate stays on the thread that owns its scene index
(Mantle's IPR, whose viewer picks from the UI thread) cannot pick: `pick`
is on the `Delegate`, which holds `Rc` scene-index state and never leaves
that thread, while every other poll already goes through the `RenderHandle`.

## What Changes

- `RenderHandle::pick`, answerable from any thread, with `Delegate::pick`'s
  results and `Busy` cases; `Delegate::pick` delegates to it.
- The handle shares the committed scene with the delegate; an update takes
  it back whole, so updates still apply in place.

## Capabilities

### New Capabilities
None.

### Modified Capabilities
- `render-delegate`: `pick` is one of the polls any thread may make.

## Impact

- `crates/crust-render/src/delegate/mod.rs`, `tests/delegate.rs`.
- `openspec/specs/render-delegate/design.md` (the polls).
