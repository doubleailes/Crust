# Tasks

Every task ends green on `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace --no-fail-fast`. "Parity green" means
`cargo test -p crust-core --test import_parity` passes (group 3).

## 1. One openusd, and Moho as a dependency (design D1, D2)

- [ ] 1.1 Switch `[patch.crates-io]` in the root `Cargo.toml` from `branch = "main"` to
  `rev = "68a867a69f3fb429a30f9e1155a0a30a3d2cdf9f"` for `openusd` and
  `openusd-schemas`, and rewrite the comment above it: pinned by rev to Moho's, bumped in
  order Moho, then crust-render, then Mantle, and no longer moved by `cargo update -p`.
  Regenerate `Cargo.lock`. Verify that `Cargo.lock`'s three openusd entries read
  `?rev=68a867a…#68a867a…`, that `cargo tree -i openusd` shows one package, and that
  `cargo deny --locked check` is clean.
- [ ] 1.2 Add `moho = { git = "https://github.com/doubleailes/Moho", rev = "92c8b847822823f796b81b85452c3c400d77e5ec" }`
  to `[workspace.dependencies]` and `moho.workspace = true` to crust-core. Add
  `https://github.com/doubleailes/Moho` to `allow-git` in `deny.toml`, and update the
  comment there. Verify:
  - `cargo tree -i openusd` and `cargo tree -i openusd-schemas` each show a single
    package, with `moho` among its dependents;
  - `cargo tree -d` lists no openusd crate;
  - `cargo deny --locked check` is clean.
- [ ] 1.3 Add a compile-time guard test in crust-core (`tests/one_openusd.rs`) that
  passes a `moho::openusd::usd::Stage` where an `openusd::usd::Stage` is expected, and a
  `moho::openusd_schemas` type where the workspace's `openusd_schemas` type is expected.
  Verify that it compiles and passes. Then check, once, that it fails to compile with
  the patch temporarily set back to `branch = "main"`, and record that in the test's doc
  comment.
- [ ] 1.4 Update `CLAUDE.md`'s openusd paragraph: pinned by rev, the Moho entry pinned
  by rev, one openusd across Moho, crust-render and Mantle, and the bump order. Document
  local co-development with Moho through an untracked `.cargo/config.toml`
  (`[patch."https://github.com/doubleailes/Moho"] moho = { path = "../Moho/crates/moho" }`;
  the root `.gitignore` already ignores `config.toml`). Verify with `grep` that
  `CLAUDE.md` names both revs' rule and the bump order.
- [ ] 1.5 Build against Moho's `scene-index-gaps` (M1 to M7 closed) through an untracked
  `.cargo/config.toml` path patch, and bump the `moho` rev in `[workspace.dependencies]`
  to the merge of `scene-index-gaps` once it lands on Moho's `main`, dropping the local
  patch. Verify that `cargo tree -i moho` names the merged rev, and that a clean checkout
  without `.cargo/config.toml` builds and passes `import_parity`. Until the merge this
  task stays open, and the design's "Moho's rev" note says so.

## 2. The route switch (design D3)

- [ ] 2.1 Add `ImportRoute { Direct, Index }` and `Config::usd_import`, read from
  `CRUST_USD_IMPORT` in `config.rs` (default `Direct`; an unknown value warns once
  through the existing config-warning path and keeps the default). Add
  `UsdImportOptions::route: Option<ImportRoute>` overriding it per import. Record the
  effective route in `StageFacts::options`. Verify with unit tests for parsing, the
  unknown value and the override, and that every existing `UsdImportOptions { .. }` site
  compiles through `Default`.
- [ ] 2.2 Make `load_scene` dispatch on the route after `ValidOptions::check`. Until
  group 4, `Index` returns the `Error` variant for an unsupported option with a clear
  message. Verify with a test that `Direct` is unchanged: `scripts/check_images.sh
  check` passes against goldens recorded before the change.
- [ ] 2.3 Add the `CRUST_USD_IMPORT` row to `docs/architecture.md` § Environment
  switches and its section to `site/content/docs/reference/environment-variables.md`
  (marked as experimental until group 15). Verify with `zola build` in `site/`.

## 3. The parity harness (design D11, spec `import-parity`)

- [ ] 3.1 Add a test-only scene digest in crust-core (behind `cfg(test)` or a
  `parity` feature that only tests enable). It hashes:
  - geometry in id order (primitive buffers, instance transforms, material identity by
    resolved parameters, masks, holdout flags);
  - the light list;
  - camera, settings and the AOV request;
  - warnings in order, and the provenance.

  It returns a field-by-field diff naming the first difference. Verify with a unit test
  where two scenes that differ in one geometry's order report that geometry id.
- [ ] 3.2 Add `crust-core/tests/import_parity.rs`. For each committed `samples/*.usd*`
  that imports without external assets (the excluded ones listed with reasons), it
  imports on both routes with `UsdImportOptions::route` and compares digests and the
  `crust check` report data, timings excluded. Until group 4 the index side is
  `#[ignore]`d. Verify that the test lists every sample, ignored or not.
- [ ] 3.3 Add a helper the existing import tests (`usd_scene.rs`, `light_linking.rs`,
  `render_passes.rs`, `mesh_primvars.rs`, `camera_*.rs`, `motion_vector.rs`, ...) call
  in place of a bare import: it imports on both routes and asserts parity before
  returning the direct scene. Convert the files one by one. Verify by `grep` that no
  import test in `crust-core/tests` calls the importer without it, apart from a short,
  commented list.
- [ ] 3.4 Add `scripts/import_parity.sh <scene> [render flags]`. It renders on both
  routes interleaved (as `bench_ab.sh` does, `-n` runs), at `-s 16 --indirect-clamp 0`,
  with `--stats`, and compares `crust check --json` minus timings and the EXRs with
  `crust diff`. It prints each route's peak memory and `Parse USD stage` time, and the
  ratios, and exits non-zero on any report or image difference; memory and time are
  reported, not gated (as UsdImaging's scene index switch was). Verify on
  `samples/cornellbox.usda` once group 4 lands (exit 0), and on a deliberately broken
  index route (exit non-zero).
- [ ] 3.5 Record the baselines: the direct route's ALab (`samples/ALab/entry.usda -f 1004`)
  and Moana (`--camera /island/cam/shotCam`) peak memory and import time, and Moho's
  `scripts/streaming-spike.sh --with-d` strategies (B) and (D) on Moana, on one machine, in
  `openspec/specs/usd-scene-import/design.md` under a new "Index import" section.
  Verify that the section names the machine, the revs and the commands.

## 4. The index skeleton and the bridge (design D4, D5, D6, D7, D10)

- [ ] 4.1 Create `usd_import/index/` (`mod.rs`, `walk.rs`, `bridge.rs`,
  `stage_facts.rs`), importing openusd through `moho::openusd` and
  `moho::openusd_schemas` (D2). Build the bridge chain: `StageSceneIndex` →
  `Flattening` → `MaterialBindingResolving` → `LightLinking` →
  `RenderSettingsFlattening`, with no instancing filters (D6). Every stage is opened
  with Crust's `stage_builder()` and set to the import's time (`TimeCode::default()` for
  none). Verify with a unit test that a chain over `samples/cornellbox.usda` lists its
  prims with their Hydra types.
- [ ] 4.2 Single stage: wrap the `LoadAll` stage in one chain. Walk from the absolute
  root with the direct route's stack discipline. Apply pruning from the flattened
  visibility and purpose, keeping the camera search in invisible subtrees. Hand every
  prim to the existing readers through the bridge (`StageSceneIndex::stage()` and the
  same path, under `EvalTimeScope`). Verify that `import_parity` passes on every
  single-stage sample with the index side un-ignored.
- [ ] 4.3 Streaming: build the `MergingSceneIndex` with the scene input (`LoadNone`,
  `set_include_unloaded_prims(true)`) and one chunk input at a time (masked to the chunk
  and the active pass prim). Add the input, walk from the root through that chunk's root
  only, remove it, drop its stage, and advance the epoch as today. The bridge resolves
  each path to the stage of the input that holds it. Verify:
  - parity on every streamed sample and on a fixture with 5 top-level subtrees;
  - with `CRUST_STREAM_IMPORT=0`, a test that the streamed and single-stage index
    routes are equal.
- [ ] 4.4 Move the non-imaging stage facts (D7) behind `index/stage_facts.rs`: layers
  and layer stack, and invisible overrides, each with a comment naming the reason it is
  not imaging data. Read the time range's authored-ness (`check_time_range`) from the
  root's `__usdStageMetadata/{startTimeCode,endTimeCode}/authored` (M3), not from the
  stage. Verify that `Scene::layers` and `Source` are equal on both routes in
  `import_parity`, and that the time-range warnings are equal on a stage with and one
  without an authored range.
- [ ] 4.5 Make `crust check`, `crust diagnostic` and the session's import honour the
  route through `UsdImportOptions`. Verify with a crust-render test that `check --json`
  on `samples/cornellbox.usda` is equal under both values of `CRUST_USD_IMPORT`.

## 5. Settings, products, precomp, passes and camera

- [ ] 5.1 Index reader for render settings: `renderSettings` from the scene input
  (camera, resolution, pixel aspect, conform policy, data window, motion and depth of
  field toggles, `renderingColorSpace`), and `crust:*` from `namespacedSettings`. The
  active settings come from the scene globals and the host's pass. Replaces
  `read_index`'s settings reads on the index route only. Verify parity green, plus a
  test on `samples/aovs.usda` and the settings fixtures.
- [ ] 5.2 Index reader for products and vars (flattened `renderProducts`,
  `driver:parameters:*`, `ri:*`, `crust:aov:*`, `crust:filter*`), and precomp filters
  read from the generic data of the prims the settings' relationships target. Verify
  parity green on `aovs.usda`, `aovs_lpe.usda`, `primvar_aovs.usda` and `precomp.usda`.
- [ ] 5.3 Index reader for the active pass: `renderPass` and its `collections`
  (`matte`, `cameraVisibility`, and `prune`/`renderVisibility` for the existing warning
  that they are not honoured, M6). Membership is judged per chunk at the instance or
  instancer path, as today. Verify parity green on `crust-core/tests/render_passes.rs`
  and `holdouts.rs`, including a pass that applies all four collections.
- [ ] 5.4 Index reader for cameras: the `camera` schema (projection, apertures and
  offsets, clipping, focus, shutter, exposure), `metersPerUnit` and its authored-ness
  from the root's `__usdStageMetadata/metersPerUnit/{value, authored}` (M3), and the
  dicing camera. Verify parity green on `camera_*.rs`, `aspect_conform.rs` and
  `orthographic` fixtures, with `metersPerUnit` authored and not.
- [ ] 5.5 Move the settings-and-camera rebuild in `usd_import/live.rs` to the index
  readers on the index route (D8). Verify that `crust-core/tests/live.rs` passes under
  both routes, with the in-place versus re-import bitwise checks.

## 6. Transforms and motion

- [ ] 6.1 Take world matrices from the flattened `xform` on the index route
  (`resetXformStack` honoured), in place of `compose_with_parent`. Take `Timing` and
  spans from `sample_times` over the shutter offsets and values at each offset, plus
  `crust:motion:translate` from generic data. Verify parity green on
  `motionblur.usda`, `motionvector.usda`, `animation.usda` at `-f`, and the transform
  stack fixtures in `usd_scene.rs`.
- [ ] 6.2 Move the "time-sampled motion not produced" reporting to the index route's
  sample times. Verify that the warnings are equal on both routes in the motion
  fixtures.

## 7. Meshes

- [ ] 7.1 Index reader for `mesh`: topology, holes, orientation, subdivision scheme and
  tags, `doubleSided`, and points and normals (`primvars:normals` precedence) read once
  as whole arrays. It feeds `MeshSource` and the existing bake and instance placement
  path. Verify parity green on `cornellbox.usda`, `displacement.usda`, the subdivision
  fixtures and `mesh_normals.rs`.
- [ ] 7.2 Primvars: the render vars' primvars and materials' primvars with indices,
  interpolation, `elementSize` and colour space, de-indexed as the direct route does.
  Constant primvars are read from the prim's own generic data, not the flattened
  (inherited) set (design, Risks). Verify parity green on `mesh_primvars.rs`,
  `primvar_aovs.usda` and `ptex_quads.usda`.
- [ ] 7.3 `crust:subdivisionLevel`, `crust:subdivisionEdgeLength`,
  `crust:displacementBound` and `primvars:displacementbound:sphere` from generic data,
  and the adaptive level from the camera's projection. Verify parity green on the
  adaptive subdivision fixtures and `pxr_displace.usda`.
- [ ] 7.4 Move the mesh parts of `usd_import/live.rs` (`mesh_source`, bake helpers) to
  the index reader on the index route. Verify that `live.rs` and the
  `live-geometry-edits` tests pass under both routes.

## 8. Implicit shapes and spheres

- [ ] 8.1 Index readers for `sphere`, `cube`, `cone`, `cylinder`, `capsule` and `plane`
  (radius, size, height, axis, radiusTop/Bottom, width, length, doubleSided), tessellated
  by Crust's existing `implicit` code. Analytic spheres stay analytic. Verify parity
  green on `implicit_shapes.usda`, and that `geomutil_oracle` passes unchanged.
- [ ] 8.2 Move the sphere rebuild in `live.rs` to the index reader on the index route.
  Verify that `live.rs` passes under both routes.

## 9. Curves

- [ ] 9.1 Index readers for `basisCurves` and `nurbsCurves` (topology, widths, order,
  knots, ranges, point weights) and the `primvars:curve*` and `st` primvars that strand
  requests read. Verify parity green on `curves.usda`, `hair.usda`,
  `hair_variation.usda`, `curve_attributes.rs` and `hair.rs`.

## 10. Materials

- [ ] 10.1 Binding: read `materialBindings` for the render purpose, then the
  all-purpose binding, from the material binding resolving filter. Check it against
  `winning_binding_target` with a test that runs both on `purpose_bindings.usda` and the
  binding fixtures. Verify parity green.
- [ ] 10.2 Networks: `material` per render context (universal, `mtlx`, `ri`). Terminals,
  `nodeIdentifier`, parameters with colour space and resolved asset paths, and
  connections feed `preview` (`UsdPreviewSurface`, `UsdUVTexture`, primvar readers),
  `mtlx_network` (inline `ND_*`), `PxrDisneyBsdf` and `PxrDisplace`. `crust:openpbr` is
  read from the material's generic data. Verify parity green on every `materialx_*`,
  `openpbr_showcase.usda`, `ptex_quads.usda` and `mtlx_surfaces.rs`.
- [ ] 10.3 `.mtlx` references: on the index route, read the arc through `stage_facts`
  (D7) as `mtlx_reference` does, unless `mtlx-format-plugin` has landed, in which case
  the composed shader prims are read like any network. Verify parity green on
  `materialx_teapot.usda` and `materialx_lion.usda`.
- [ ] 10.4 Materials outside a chunk: add the side input (D4), a `chain::input` over a
  stage masked to the paths `moho::streaming::unresolved` lists (M2), inserted weaker
  than the chunk and stronger than the scene input, grown and reopened under
  `OutsideMaterials`' rules (at most `MAX_SIDE_STAGE_OPENS` per material) until nothing
  is unresolved, and removed after the last chunk. Verify:
  - parity on a fixture with `/World/Looks` beside four geometry chunks;
  - parity on a fixture whose material lives only in another chunk's payload;
  - parity on the Cornell box streamed.
- [ ] 10.5 Provenance for materials (`material_reads`: arcs from `prim_stack`, targets
  from the generic `connections`) on the index route, through `stage_facts` for the
  arcs. Move the material rebuild in `live.rs` to the index readers. Verify that
  `live.rs` passes under both routes, and that provenance is equal in `import_parity`.
- [ ] 10.6 Run `scripts/import_parity.sh` on ALab. Verify that it exits 0, and record
  the run in the design record.

## 11. Lights and light links (design D9)

- [ ] 11.1 Index readers for every light type through the `light` container
  (intensity, exposure, colour and temperature, normalize, the type's own inputs, dome
  texture and pole axis, shaping and IES), plus `crust:light:*` from generic data. They
  feed the existing `emit_*_light` builders. Verify parity green on `lights.rs`,
  `domelight.usda`, `dome_backdrop.usda`, `rectlight.usda`, `many_lights.usda`,
  `light_visibility.usda`, `hidden_lights.rs` and the IES samples.
- [ ] 11.2 Light links: record receivers as today. Read each light's
  `collections/lightLink` and `collections/shadowLink` expressions from the merged data,
  and judge them after the last chunk without reading `categories` (D9; whether to read
  `categories` now that M1 has landed is the open question). Add agreement tests
  asserting that Moho's `categories` agree with Crust's answers: on a single stage
  (`chain::standard`), and on a streamed fixture whose light and receivers sit in
  different chunks (`chain::merged` over every input). Verify parity green on
  `light_linking.usda` and `light_linking.rs`, and on that streamed fixture.
- [ ] 11.3 Move the light rebuild in `live.rs` to the index readers. Verify that
  `live.rs` passes under both routes.

## 12. Volumes

- [ ] 12.1 Index reader for `crust:volume:*` regions from generic data, dispatched
  before geometry as today, and the material's volume terminal from the `material`
  network. Verify parity green on `fog.usda`, `materialx_volume.usda` and `medium.rs`.

## 13. Instancing, the standard chain, and the bridge's removal

- [ ] 13.1 Switch the index route to `moho::chain::standard` (one stage), and to
  `chain::input` per input under `chain::merged` (streamed). Read `instancer` prims:
  - native instances: `instancerTopology` and `hydra:instanceTransforms`, with
    prototypes walked under their stable paths through the same readers;
  - point instancers: raw `protoIndices`, positions, orientations, scales and
    invisible/inactive ids from `__usdAttributes` where a warning depends on them, and
    `instancerTopology` otherwise.

  Key prototype caches on the stable prototype paths on the index route, with no epoch.
  Name prims inside a prototype, in warnings and provenance, by the instance's prototype
  joined with `primOrigin/scenePath` (M4). Verify parity green on `instancing.usda`,
  `nested_instancing.usda`, `PointInstancedMedCity.usd` and the `usd_scene.rs`
  instancing tests, with no path normalisation; a remaining difference is reported, not
  normalised.
- [ ] 13.2 Placement counts for adaptive subdivision and the bake-versus-instance
  decision from instancers, per top-level subtree. Instances judged at the instance or
  instancer path for links and passes. Verify that parity is green with adaptive
  subdivision on, on the instancing samples, streamed and not.
- [ ] 13.3 Delete the bridge and `EvalTimeScope` use from the index route. Every index
  reader reads only the scene index or `stage_facts`. Verify with `grep` that
  `index/` names no `usd::Prim` outside `stage_facts.rs`, and that parity is green.
- [ ] 13.4 Run `scripts/import_parity.sh` on ALab and Moana. Verify that both exit 0,
  and record the runs (and Moho's spike strategies (B) and (D) on the same machine) in
  the design record.

## 14. Hosts and large scenes

- [ ] 14.1 hdCrust: run `hydra/hdCrust/test/compare.sh`, `interaction.sh` and
  `usdc_roundtrip.sh` with `CRUST_USD_IMPORT=index`. Verify that all three pass.
- [ ] 14.2 Run `crust diagnostic --budget 2m` and the MCP session tests
  (`crust-render` with feature `mcp`) under both routes. Verify equal reports, timings
  aside, and green tests.
- [ ] 14.3 Run `scripts/check_images.sh check` with `CRUST_USD_IMPORT=index` against
  goldens recorded on `direct`. Verify exit 0.

## 15. The default flips

- [ ] 15.1 Change `Config::usd_import`'s default to `Index` (keeping `direct`
  selectable), with ALab and Moana re-run on this commit by `scripts/import_parity.sh`.
  Verify that both exit 0 and the full CI is green.
- [ ] 15.2 Mark the requirement "Two import routes, chosen per import" as implemented
  (drop its status note), and update the environment-variables page and the
  architecture row to the new default. Verify with `zola build` and
  `openspec validate moho-import --strict`.

## 16. Documentation

- [ ] 16.1 Update `openspec/specs/usd-scene-import/design.md`:
  - the index route (layout, walk order, bridge history, stage facts);
  - the parity runs;
  - Moho's gaps M1 to M7, closed in `scene-index-gaps`, with the Moho rev that closes
    them, and any parity exception that remains (none is planned).

  Verify that M1 to M7 are each named with what Crust uses from them.
- [ ] 16.2 Update `docs/architecture.md`: Moho in the crate map and render flow, the
  `index/` module, and the new invariant that each family's index reader and its
  in-place rebuild change together. Update the `usd_import/mod.rs` module table to
  name each family's index reader. Verify with `grep` that each document names Moho
  and the rev rule.
- [ ] 16.3 Add a page on the import routes to the site's architecture section (what
  the index route is, `CRUST_USD_IMPORT`, and that the direct route is kept for A/B).
  Verify with `zola build`.
