## 1. HostFacts asks one thing (design D1)

- [x] 1.1 Remove `time_range`, `layers`, `layer_stack`, `invisible_overrides`, `not_a_pass`, `foreign_spec_paths`, `xform_facts` and `instance_without_prototype` from `HostFacts`, `NoHostFacts` and `StageFacts`, and their stage helpers no longer called (keeping `stage_facts::layer_stack` and `invisible_overrides` for 3.1)
- [x] 1.2 Remove `Scene::layers` and its assignment
- [x] 1.3 Drop the foreign spec paths from material provenance reads (`hydra/materials.rs`)
- [x] 1.4 Add a test implementing `HostFacts` with only `mtlx_reference` (spec "One question")

## 2. Warnings from the index (design D2)

- [x] 2.1 `check_time_range` reads `sceneGlobals/startTimeCode` and `endTimeCode` from the index, `0..0` unauthored; tests for a frame outside an authored range and for no range, both with `NoHostFacts` (spec "A frame outside the range", "No range authored")
- [x] 2.2 `Error::PassNotFound` explains from the index (no prim, or its type: the USD type name from `__usdPrimInfo`, else its Hydra type); a test with a pass naming a mesh (spec "A pass that is a mesh")
- [x] 2.3 Remove `xform_diagnostics` and the codes `xform.unknown_op`, `xform.unknown_type`, `xform.uncomposable`, `instancing.instanceable_without_prototype` from `warnings.rs`, their rows from `site/content/docs/reference/warnings.md`, and their tests; the warnings-in-step test passes

## 3. The stamp from the batch host (design D3)

- [x] 3.1 `usd_import::load_scene_with` reads the layer stack and the invisible overrides from its stage before the commit and sets them on the scene's `source`; `commit_scene` no longer sets them
- [x] 3.2 Verify `crust-render/tests/stamp.rs`'s stamp cases and `crust diff`'s comparability notes are unchanged (spec "The stamp after a render"), on Linux CI where the Windows path-spelling case fails before this change too

## 4. Report version, docs and gate (design D4)

- [x] 4.1 `crust-check/2` → `crust-check/3` in `check.rs`, its tests, and `site/` (the command-line reference and any `crust-check/2` mention)
- [x] 4.2 Docs: `site/content/docs/usd/geometry.md` (no xform warnings), the `render-delegate`, `usd-scene-import` and `scene-warnings` design records (host facts: the `.mtlx` reference only; the `0..0` gate), `docs/architecture.md`'s `HostFacts` row
- [x] 4.3 `crust-render/tests/host_facts.rs` still passes; `scripts/check_images.sh check` against goldens recorded before the change (16 spp, `--indirect-clamp 0`) reports every sample identical
- [x] 4.4 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p crust-render --features delegate --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast`; verify all pass (on Linux CI for the Windows-only cases)

## Workflow follow-up

- Mantle's `ipr-index-only` follows: its `CrustDelegate` answers `mtlx_reference` from the mirror stage.
