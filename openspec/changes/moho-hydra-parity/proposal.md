## Why

Moho, the scene index Crust imports through, has gone to strict Hydra 2.0 /
UsdImaging parity (Moho's `hydra-parity-stage`, `-instancing`, `-materials`,
`-chain` and the final check, merged at `510174c`). It no longer streams, it lays
out instancing as UsdImaging does, it has dropped M10 and M12, it resolves
materials and bindings as UsdImaging does, it moved light linking and dependency
forwarding into renderer filters, and it offers the Hydra 2.0 consumer path (notice
batching by prim type and a prim managing observer). Crust pins Moho at `d09cc94`
and still depends on everything that was dropped.

The parity work had a second purpose: Crust should be a Hydra 2.0 render delegate.
Its per-prim import should be the shape of hdPrman's Riley path
(`SetTerminalSceneIndex` and `Update`), so that the same Crust code runs over Moho in
Rust and, later, over Hydra's terminal scene index through hdCrust in usdview and
Solaris. S2 (`moho-import`) also grew the codebase by about 4 700 lines of code and 74 000
lines of baselines, to prove parity with an importer that no longer exists. This change
is where the code shrinks.

## What Changes

- **Moho bump** to `510174c` (Moho main). openusd still comes only through
  `moho::openusd`.
- **Crust becomes a Hydra 2.0 render delegate over Moho.** The import is a prim
  factory (`moho::prim_managing`) behind `PrimTypeNoticeBatchingSceneIndex`, over
  `chain::standard` and `chain::renderer_filters(index,
  &[RendererFilter::light_linking(), RendererFilter::dependency_forwarding()])`.
  Managed prims hold what Crust built for each prim. A commit after each `Update`
  (flush) builds or updates the renderer's scene. A batch render (`crust render`,
  `check`, `diagnostic`) is one population, one flush and one commit. A live session
  is the same delegate kept alive.
- **Crust reads only Hydra schemas.** It reads no Moho generic data
  (`__usdAttributes`, `__usdRelationships`), because in Solaris and usdview it will
  not exist. **BREAKING** for scenes:
  - geometry and volume `crust:*` attributes become constant primvars
    (`primvars:crust:rayMask`, `primvars:crust:volume:type`, …), as hdPrman's
    `primvars:ri:attributes:*` and arnold-usd's `primvars:arnold:*` are;
  - render settings, product and var `crust:*` attributes are read from Hydra's
    `namespacedSettings`;
  - `crust:*` relationships on those prims (`crust:dicingCamera`,
    `crust:sampleFilters`, `crust:displayFilters`, `crust:filter`) become `string`
    path attributes, because Hydra carries no namespaced relationship;
  - light `crust:light:*` attributes stay as they are, read through the `light`
    container by name;
  - precomp sample and display filter graphs become `Material` prims with
    `outputs:crust:*` terminals.
  `crust check` reports a legacy `crust:*` attribute or relationship that is no
  longer read.
- **Single-stage import.** **BREAKING**: `CRUST_STREAM_IMPORT` is removed, along
  with the streamed chunks, the side stages, the chunk inputs and their caches.
  Crust keeps no chunking of its own.
- **Hydra's instancing layout.** Native prototypes are read at
  `UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer/UsdNiPrototype`,
  and point instancer prototypes from their `ForInstancer<hash>` copies, through
  `instancerTopology`, `instancedBy` and the instance primvars. With M12 gone, a
  point instancer target inside a native instance is empty, as in UsdImaging 2.0.
  Moana's `isCoral` coral prototypes therefore render nothing. This is accepted and
  listed as a parity exception against the old direct importer.
- **Light linking from Hydra's categories.** A geometry's light and shadow links
  come from its `categories` (or its instancer's `instanceCategories`), matched
  against each light's `light/lightLink` and `light/shadowLink` ids (`group_<n>`).
  Crust no longer judges collections for light linking.
- **Missing means default.** Any data Hydra does not give is read as its default:
  an absent `xform` is the identity, an absent `visibility` is visible, an absent
  `purpose` is `default`. A native instancer may or may not carry flattened data
  (Moho's parity check, "exempted Hydra artifacts").
- **Time is the index's.** Without `-f`, the stage is read at time code 0, as a new
  `UsdImagingStageSceneIndex` reads it, rather than at its default values.
  **BREAKING** for stages that author both a default and time samples.
- **Live updates come from dirty locators.** The session keeps one stage and one
  delegate. Each owned layer's change is replayed onto that stage as a `usd::Diff`,
  and the dirtied locators decide what is rebuilt. The session's classes
  (`settings`, `materials`, `lights`, `geometry`, `full`) stay as the reported
  outcome. Adding or removing a light becomes `lights` rather than `full`.
- **Parity machinery removed.** Deleted:
  - `crates/crust-core/tests/data/import_baselines/` (3 603 files);
  - `crates/crust-core/src/parity.rs`, the `parity` feature, and the digests in
    `rt_world.rs` and `live.rs`;
  - `crates/crust-render/tests/import_parity.rs`;
  - `scripts/import_parity.sh`, `scripts/parity_images.sh` and
    `scripts/parity_images.expected`.
  The `import-parity` capability is retired. Image goldens
  (`scripts/check_images.sh`) remain the regression guard.
- **Update latency is measured, not gated.** Crust adds an update-latency benchmark
  for four edits: a camera's `focalLength`, a camera's transform, adding a light with
  trivial collections, and adding a light linked to one prim. It measures each from
  the edit to the restarted render, on a fixture and on Moana, starting where Moho's
  benchmark stops (Moho's `docs/update-latency.md`). First-load time is re-measured.
- **hdCrust stays on its layer path in this change.** It still writes layers that
  this importer reads, and its session gains the new update path through
  `LiveSession`. Making hdCrust run the delegate over Hydra's own terminal scene
  index is the next change (`hdcrust-scene-index`, design D10).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `scene-index-import`: the import is a Hydra 2.0 delegate over Moho that reads only
  Hydra schemas. It reads one stage, and the time defaults to code 0. The
  requirements about streaming and about parity with the direct importer are
  replaced by Hydra parity, a listed set of exceptions, renderer attributes carried
  as Hydra carries them, and missing data read as its default.
- `import-parity`: retired. Every requirement is removed.
- `usd-scene-import`: affected requirements:
  - loading, light linking, volumes, native nesting and unread geometry types are
    read through Hydra's data;
  - streaming is gone from geometry mapping, light mapping and adaptive subdivision;
  - `crust:*` geometry attributes are authored as primvars.
- `mcp-session`: in-place edits are decided from the dirtied locators. A light
  added or removed is `lights`. Update latency is reported.
- `displacement`: determinism no longer mentions streaming.
- `render-passes`: pass collections are Hydra's `collections`, evaluated over the
  index. The streamed-chunk gap is gone.

## Impact

- **Code.**
  - `crates/crust-core/src/scene/usd_import/`: the Moho host, which opens the
    stage, builds the chain and reads stage facts.
  - A new `crates/crust-core/src/scene/hydra/`, which holds the delegate: the
    factory, the managed prims, the commit and the per-family readers moved there.
  - `crates/crust-core/src/live.rs` keeps the provenance and loses the masked-stage
    rebuilds.
  - `crates/crust-render/src/session/` replays diffs onto the held stage.
  - `crates/crust-core/src/rt_world.rs` and `config.rs` lose the parity digest and
    `CRUST_STREAM_IMPORT`.
  - `hydra/hdCrust/sceneWriter.cpp` writes `crust:dicingCamera` as the string
    attribute.
  - `Cargo.toml` and `Cargo.lock` take the Moho rev.
- **Samples and fixtures.** Every `crust:*` geometry or volume attribute in
  `samples/*.usda` and in the test fixtures moves to `primvars:crust:*`.
  `samples/precomp.usda` moves to `Material`-based filters and string paths.
- **Docs.**
  - `CLAUDE.md` and `docs/architecture.md`: crates, the render flow, the Invariants
    pairs on prototype keys and on in-place updates, the environment switch table.
  - The design records: `usd-scene-import` § Index import, `mcp-session` (update
    latency) and `cli` (cookbook).
  - The site: `reference/environment-variables.md`, `reference/warnings.md`,
    `usd/geometry.md`, `usd/volumes.md`, `usd/render-settings.md`,
    `usd/render-passes.md`, `architecture/usd-import.md`.
- **Performance.**
  - Render throughput is unchanged: the same kernel and the same BVH builder.
  - Geometry and light ids follow path order. Images change at noise level, and the
    goldens are re-recorded after a 1/√N check.
  - First-load time and memory are re-measured on ALab and Moana and reported
    against the S2 figures (ALab 1.93× memory, Moana 12.5× import time). Without
    streaming, Moana composes whole: Moho's update benchmark composes it `LoadAll`
    at a 7.01 GiB peak.
- **Size.** Net about −2 900 lines (±1 500) of Rust and scripts, and −74 155
  lines (−23 MB, −3 603 files) of baselines (design, "Size").
