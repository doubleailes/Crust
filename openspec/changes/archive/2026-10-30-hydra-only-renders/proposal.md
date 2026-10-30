## Why

Crust's delegate reads only Hydra schemas (`tests/hydra_only.rs`), but three
answers it takes from the host's stage, through `HostFacts`, still change the
image: the directory of the layer that authors an asset value
(`authoring_layer_dir`), the stage's `metersPerUnit` (depth of field), and a
material's `.mtlx` reference. Under a host with no stage (`NoHostFacts`), a
relative texture authored as a `string`, a metre-unit scene's lens and a
`.mtlx`-referenced material render differently. A Hydra 2.0 delegate should
render from its index alone; the stage should only explain what it rendered.

## What Changes

- **Asset paths come from the index.** `HostFacts::authoring_layer_dir` is
  removed. An `asset` value already arrives resolved (Moho anchors it on its
  authoring layer, as UsdImaging does), and is used as it does today. A
  relative path authored as a `string`, which USD never anchors, resolves
  against the stage path's directory, as when no layer is known today, and
  raises a new warning, `asset.string_path`. **BREAKING (behaviour)**: such a
  path in a sublayer or reference in another directory no longer finds its
  file beside that layer (it did since `0bebb347`); authoring it as an
  `asset` restores it.
- **`metersPerUnit` comes from the index.** Depth of field reads
  `__usdMetersPerUnit/metersPerUnit` on the index root (Moho's
  `stage-meters-per-unit`, a prerequisite), and `HostFacts::meters_per_unit`
  is removed. Its warnings (unauthored, refused) are unchanged.
- **MaterialX references stay a host fact, until `mtlx-format-plugin`.**
  `mtlx_reference` is the one render-affecting fact left; the in-flight
  `mtlx-format-plugin` change removes it by composing `.mtlx` documents into
  the stage. This change records it as the only exception.
- **A guard.** Each sample that references no `.mtlx` renders bit-identically
  at 16 spp with `NoHostFacts` and with `StageHostFacts`.
- The remaining facts stay: they feed warnings, `crust check` and provenance
  only. `time_range` among them: its values already come from the index's
  `sceneGlobals`, and only whether the stage authors a range (Hydra gives `0`
  when it does not) is asked of the host, for the frame-outside-range
  warning. Likewise `layers`, `layer_stack`, `invisible_overrides`,
  `not_a_pass`, `foreign_spec_paths`, `xform_facts` and
  `instance_without_prototype`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `render-delegate`: a requirement that the delegate's image depends on the
  scene index alone, host facts informing diagnostics only, with
  `.mtlx` references the one listed exception.
- `usd-scene-import`: a relative asset path authored as a `string` resolves
  against the stage's directory with an `asset.string_path` warning, rather
  than on its authoring layer.

## Impact

- `crust-core`: `scene/hydra/index/host.rs` (two methods removed),
  `usd_import/stage_facts.rs`, `scene/hydra/{assets,lights,materials,products,
  camera,mod}.rs` and `precomp`, `warnings.rs` (one code),
  `tests/asset_anchoring.rs` (the string case rewritten), a new guard test.
- Moho pin: bumped to the merge of `stage-meters-per-unit`.
- Docs: `site/content/docs/reference/warnings.md`, the `usd-scene-import` and
  `render-delegate` design records, `docs/architecture.md` (the delegate's
  host facts).
- Hosts: Mantle and the batch host keep passing `StageHostFacts`; nothing
  they call changes. Mantle's mirror stage has no file, so a `string` path
  there resolves against its fixed path; Mantle's LOPs author absolute paths.
- Performance: none expected; the index reads replace stage reads once per
  commit.
