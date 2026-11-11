## Why

Mantle, the interactive host Crust's live render delegate was built for, is cancelled.
Crust's only host is now its own command line, which opens a stage, commits the scene
once and renders. Everything that existed to keep a delegate across a host's edits
(in-place updates, update classes, render control, kept decodes, placement tools) has
no caller, and keeping it costs: every import records provenance it never reads, the
world can keep a second copy of its geometry, and every change to the import has to
keep an in-place twin bit-identical to the commit.

## What Changes

- **BREAKING** Remove `crust-render`'s `delegate` feature: the `Delegate` render
  control (restart, snapshot, pick), its test and the `update_latency` example.
- **BREAKING** `crust_core::SceneDelegate` commits afresh on each call
  (`SceneDelegate::commit`). Remove `update`, `prepare`, `Updated`, `UpdateClass`, the
  notice classification and every in-place rebuild (`scene/hydra/update.rs`).
- **BREAKING** Remove `crust_core::live` (`Provenance`, `Structural`, and
  `Scene::provenance`) and `crust_core::place` (drop and scatter placement).
- Remove the world's kept geometry (`WorldBuilder::keep_inputs`,
  `UsdImportOptions::keep_geometry`), its in-place geometry, material and light-layer
  replacement, the light list's in-place replacement, and the shared environment maps
  a kept delegate reused across commits.
- **BREAKING** Remove `crust-assets`' kept decodes (`FileAssets::keep_decoded`,
  `revalidate`, `sweep`, `decodes`).
- Remove the `CRUST_SCENE_DELTAS` environment switch.
- The light layer keeps its own kernel segment, so the BVH, and every image, is
  unchanged.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `render-delegate`: a delegate commits once per call; the in-place update, render
  control, latency and re-commit requirements are removed.
- `scene-index-import`: no host keeps a delegate across edits.
- `usd-scene-import`, `displacement`, `materials`: scenarios and clauses about a kept
  delegate's updates are removed.

## Impact

- Code: `crust-render` (`delegate/`, `Cargo.toml`, `lib.rs`), `crust-core`
  (`live.rs`, `place.rs`, `scene/hydra/update.rs`, `delegate.rs`, `commit.rs`,
  `rt_world.rs`, `light/list.rs` and the import's provenance), `crust-assets`
  (`kept.rs`).
- Tests: `crust-core/tests/live.rs`, `place.rs` and `common/`, and
  `crust-render/tests/delegate.rs`, removed with what they tested.
- CI: the delegate clippy and test steps.
- Documentation: `CLAUDE.md`, `docs/architecture.md`, the `render-delegate` design
  record and the user site's environment-variable page.
- Output: none. Images are bit-identical (`scripts/check_images.sh`).
