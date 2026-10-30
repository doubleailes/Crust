## Why

Since `hydra-only-renders`, Crust's image depends on the scene index alone, but
the delegate still asks its host eight questions about the stage
(`HostFacts`): whether a time range is authored, the composed layers, the root
layer stack, `invisible` overrides, why a pass is missing, a material's foreign
spec paths, the transform facts behind three `xform.*` warnings, and whether an
instance lacks a prototype. A Hydra 2.0 delegate is handed a terminal scene
index and nothing else. The one fact Hydra cannot carry is the `.mtlx`
document a material references, which openusd does not compose until
`mtlx-format-plugin`. Every other question goes, so that Hydra is the
delegate's only input, MaterialX references excepted.

## What Changes

- **`HostFacts` keeps one method, `mtlx_reference`.** A test pins it. The
  delegate's commit, its in-place updates and its readers ask nothing else.
- **Read from the index instead:**
  - `time.outside_range` reads `sceneGlobals/startTimeCode` and `endTimeCode`.
    A range of `0..0`, what the index gives a stage that authors none, is
    unauthored, and so is a start after its end (a start authored alone); no
    frame is outside either.
  - A pass that is not a `RenderPass` (`Error::PassNotFound`) is explained by
    the index: no prim there, or the prim's type (its USD type name from the
    index's prim info, else its Hydra type).
- **Moved to the batch host.** The EXR stamp's `crust:layerStack` and
  `crust:invisibleOverrides` are the stage's, not the scene index's.
  `crust render`, which opens the stage, reads them from it and sets them on
  the scene after the delegate commits. A host that hands the
  delegate only an index (Mantle) stamps neither, as before.
- **Removed:**
  - **BREAKING (report):** the warnings `xform.unknown_op`, `xform.unknown_type`,
    `xform.uncomposable` and `instancing.instanceable_without_prototype`. The
    index gives a composed matrix and no instanceable flag, so none of them can
    be told from it. Removing codes bumps the report that carries warnings,
    `crust-check/2` to `crust-check/3` (scene-warnings' versioning rule).
  - `Scene::layers`, which nothing reads.
  - The foreign spec paths in material provenance. Provenance keeps the paths
    the index names.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `render-delegate`:
  - "The image depends on the scene index alone" now also says that the host is
    asked only for a material's `.mtlx` reference.
  - Two new requirements: the warnings the delegate raises come from the
    index, and the batch host stamps the stage's layers.
- `scene-check`: the JSON report's format is `crust-check/3`, after the four
  codes' removal (scene-warnings' versioning rule).

## Impact

- `crust-core`:
  - `scene/hydra/index/host.rs` and `usd_import/stage_facts.rs` (`StageFacts`
    answers `mtlx_reference` only);
  - `scene/hydra/{mod,settings,xform,commit,instancing,materials}.rs`;
  - `usd_import/mod.rs` (the batch host sets the stamp's two source fields);
  - `scene.rs` (`Scene::layers`), `warnings.rs`, `check.rs` (format).
- `crust-render`: the guard test (`host_facts.rs`) still holds. The stamp tests
  keep their layer stack through the batch host.
- Docs: `site/content/docs/reference/warnings.md` (four rows removed) and
  `site/content/docs/usd/geometry.md` (xform warnings). Also the
  `render-delegate`, `usd-scene-import` and `scene-warnings` design records, and
  `docs/architecture.md`.
- Hosts: Mantle passes `StageHostFacts` today and keeps doing so. With one
  method it could pass a facts object of its own, which Mantle's
  `ipr-index-only` decides.
- Performance: the stage walks behind the removed facts no longer run during a
  commit.
