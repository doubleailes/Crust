## 1. Pick through the render handle

- [x] 1.1 Add `RenderHandle::pick` (any thread, `Busy` during an update) sharing the committed scene with the delegate, and make `Delegate::pick` delegate to it; verify `cargo test -p crust-render --features delegate --test delegate` passes
- [x] 1.2 Add `a_pick_from_another_thread_leaves_updates_in_place`, failing when the handle keeps the scene through an update; verify it passes, and fails with the clear removed
- [x] 1.3 Record the shared scene in `openspec/specs/render-delegate/design.md` (the polls)
