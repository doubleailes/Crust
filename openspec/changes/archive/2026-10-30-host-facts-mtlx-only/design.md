## Context

`HostFacts` (`scene/hydra/index/host.rs`) has nine methods. `StageFacts`
(`usd_import/stage_facts.rs`, public as `StageHostFacts`) answers them from the
batch host's stage, and `NoHostFacts` answers nothing. Since `hydra-only-renders`,
only `mtlx_reference` changes the image. The other eight feed:

- `time_range`: the authored gate of `check_time_range` (`hydra/settings.rs`).
  The values already come from `sceneGlobals`.
- `layers`: `Scene::layers`, which nothing reads.
- `layer_stack`, `invisible_overrides`: `Scene::source`, which `stamp.rs`
  writes as `crust:layerStack` / `crust:invisibleOverrides`, and which
  `crust diff` compares.
- `not_a_pass`: the `why` of `Error::PassNotFound` (`hydra/settings.rs`).
- `foreign_spec_paths`: extra entries in a material's provenance reads
  (`hydra/materials.rs`).
- `xform_facts`: `xform.unknown_op`, `xform.unknown_type`,
  `xform.uncomposable` (`hydra/xform.rs`).
- `instance_without_prototype`: `instancing.instanceable_without_prototype`
  (`hydra/commit.rs`, `hydra/instancing.rs`).

The batch host (`usd_import::load_scene_with`) opens the stage, builds the
chain and `StageFacts`, commits once, and hands the stage to the commit to
release.

## Goals / Non-Goals

**Goals:**
- `HostFacts` has `mtlx_reference` only, pinned by a test.
- No warning, error or report field the delegate produces reads the stage.
- The CLI's EXR stamp keeps its layer stack and override count.

**Non-Goals:**
- The batch host's own stage checks (`stage_checks`: `time.default_and_samples`,
  `settings.path_across_arc`, …). They are the host's walk over the stage it
  opened, outside the delegate, and stay.
- `crust check`'s stamp fields. `crust check` writes no EXR.
- Recovering the removed `xform.*` and instancing warnings from the index.
  Hydra does not carry their causes.

## Decisions

**D1. `HostFacts` keeps `mtlx_reference` alone.** The other eight methods are
removed from the trait, `NoHostFacts` and `StageFacts`, with their stage
helpers when nothing else calls them. `layer_stack` and `invisible_overrides`
stay as `stage_facts` functions for D3. A test in `crust-core` implements
`HostFacts` with one method, so a second method fails to compile it.
*Alternative:* keep the trait and default the removed methods. That leaves the
stage path open for the next reader to use.

**D2. Warnings from the index.**
- **Time range:** `check_time_range` takes the index's `startTimeCode` and
  `endTimeCode`. The index gives USD's fallback, `0`, for an end the stage
  leaves unauthored, and does not say which ends are, so `0..0` (neither)
  and a start after its end (a start authored alone, Qodo on #432) read as
  unauthored. A stage that does author `0..0` then gets no
  `time.outside_range`, and an end authored alone reads as `0..end`, where
  the stage's own check called it unauthored. Both are narrower than the old
  gate, and the record says so.
- **Pass not found:** `Error::PassNotFound`'s `why` is "no prim" when the
  index has none at the path, else "a `<type>`": the USD type name the index's
  `__usdPrimInfo/typeName` gives (an `Xform` has no Hydra type), else the
  prim's Hydra type.
- **Removed:** the three `xform.*` codes and
  `instancing.instanceable_without_prototype`. An instanceable prim without a
  prototype imports directly, as before, silently.

**D3. The stamp's source is the batch host's.** `load_scene_with` reads
`stage_facts::layer_stack` and `invisible_overrides` from its stage before the
commit, which releases the stage, and sets `scene.source.layer_stack` and
`invisible_overrides` on the committed scene. `commit_scene` no longer sets
them. Mantle's delegate host never set a stamp; nothing changes there.
*Alternative:* `crust-render`'s `main.rs` reopens the stage for the stamp.
That is a second open of a production stage for two fields.

**D4. Report version.** Removing four codes bumps `crust-check/2` to
`crust-check/3` (scene-warnings: "Codes are versioned with the reports that
carry them"). No other report carries warnings: `crust-diagnostic/1`,
`crust-light-paths/1`, `crust-stats/1`, `crust-ls/1` and `crust-diff/1` do
not.

## Risks / Trade-offs

- [An `xformOp` typo or an unknown xformable type is now silent] → The
  transform still composes as the index composes it (identity for an unknown
  op, as before). `docs/usd/geometry.md` states the gap.
- [A stage that authors `0..0` loses `time.outside_range`] → Rare. Recorded
  in the usd-scene-import design record.
- [Consumers parsing `crust-check/2`] → The bump is the versioning rule's
  signal. `site/` documents `/3`.

## Migration Plan

1. D1 and D2 in `crust-core`. The removed codes come out of `warnings.rs` and
   `warnings.md` (kept in step by a test).
2. D3 in the batch host; the stamp tests pass unchanged.
3. D4, docs and design records.
4. Mantle's `ipr-index-only` follows. It can answer `mtlx_reference` from its
   mirror stage alone.
