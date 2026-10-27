# Tasks

Every task ends green on `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace --no-fail-fast`. "Parity green" means
`cargo test -p crust-core --test import_parity` passes, comparing today's import with the
baselines recorded from the direct importer before anything migrated (group 2).

## 1. One openusd, and Moho as a dependency (design D1, D2)

- [x] 1.1 Switch `[patch.crates-io]` in the root `Cargo.toml` from `branch = "main"` to
  `rev = "68a867a69f3fb429a30f9e1155a0a30a3d2cdf9f"` for `openusd` and
  `openusd-schemas`, and rewrite the comment above it: pinned by rev to Moho's, bumped in
  order Moho, then crust-render, then Mantle, and no longer moved by `cargo update -p`.
  Regenerate `Cargo.lock`. Verify that `Cargo.lock`'s three openusd entries read
  `?rev=68a867a…#68a867a…`, that `cargo tree -i openusd` shows one package, and that
  `cargo deny --locked check` is clean.
- [x] 1.2 Add `moho = { git = "https://github.com/doubleailes/Moho", rev = "92c8b847822823f796b81b85452c3c400d77e5ec" }`
  to `[workspace.dependencies]` and `moho.workspace = true` to crust-core. Add
  `https://github.com/doubleailes/Moho` to `allow-git` in `deny.toml`, and update the
  comment there. Verify:
  - `cargo tree -i openusd` and `cargo tree -i openusd-schemas` each show a single
    package, with `moho` among its dependents;
  - `cargo tree -d -e no-build` lists no openusd crate (openusd is also openusd-schemas'
    build-time code generator, so a plain `cargo tree -d` lists the one package twice,
    host and target, as it did before this change);
  - `cargo deny --locked check` is clean.
- [x] 1.3 Add a compile-time guard test in crust-core (`tests/one_openusd.rs`) that
  passes a `moho::openusd::usd::Stage` where an `openusd::usd::Stage` is expected, and a
  `moho::openusd_schemas` type where the workspace's `openusd_schemas` type is expected.
  Verify that it compiles and passes. Then check, once, that it fails to compile with
  the patch temporarily set back to `branch = "main"`, and record that in the test's doc
  comment.
- [x] 1.4 Update `CLAUDE.md`'s openusd paragraph: pinned by rev, the Moho entry pinned
  by rev, one openusd across Moho, crust-render and Mantle, and the bump order. Document
  local co-development with Moho through a Cargo config file passed with `--config`
  (`[patch."https://github.com/doubleailes/Moho"] moho = { path = "../Moho/crates/moho" }`).
  `.cargo/config.toml` is tracked in this repository, so the patch does not go there, and
  a path patch rewrites `Cargo.lock`'s `moho` entry, which is restored before a commit.
  Verify with `grep` that `CLAUDE.md` names both revs' rule and the bump order.
- [x] 1.5 Build against Moho's `scene-index-gaps` (M1 to M7 closed), and bump the `moho`
  rev in `[workspace.dependencies]` to its merge on Moho's `main` (`fc391a5`), dropping
  the local patch. Verify that `cargo tree -i moho` names the merged rev, that
  `cargo tree -i openusd` still shows one package, and that the workspace builds and
  passes its tests without the patch.

## 2. Baselines from today's importer (design D3, D11, spec `import-parity`)

Recorded before anything migrates, from the direct importer as it is at the start of
this group, and kept for the whole change.

- [x] 2.1 Add a test-only scene digest in crust-core (`src/parity.rs`, behind a `parity`
  feature that only tests enable, through dev-dependencies). It records, as stable text
  with the stage's directory, the workspace and the temporary directory normalised:
  - geometry in id order (every piece a ray of each category sees, hashed bit for bit,
    instances through their transforms; the material by sharing order and `Signature`;
    light class, holdout and transparent-emitter flags; side tables; scale; motion);
  - the light list (kind, name, LPE tag, pmf, shadow mask, link flags, lobe gains) and
    the volumes;
  - camera, settings, the AOV request and the pass;
  - warnings in order, and the provenance (maps sorted);
  - the scene's layers and `Source`.

  A material's parameters and a light's radiance are not in it (neither describes
  itself); the 16 spp image covers them. Comparing two digests names the first
  differing line. Verify with a unit test where two scenes that differ in one
  geometry's order report that geometry id.
- [x] 2.2 Check every import against a baseline. With the `parity` feature, every
  successful `load_scene` compares the scene's digest with the baseline under
  `crust-core/tests/data/import_baselines/`, keyed by the layers it composed (paths and
  text, normalised), the options, the environment switches and the importing test (its
  thread's name, or a `crust` process's command line), and panics naming the stage and
  the first difference. `CRUST_RECORD_BASELINES=1` records instead;
  `CRUST_PARITY_IMAGES=1` also re-imports the stage and compares a hash of its 16 spp
  image (indirect clamp off), meant for `cargo test --release`, since an unoptimised
  build renders every import of the suite too slowly. Turn the feature on for
  crust-core's, crust-render's and crust-hydra's tests, so every import test fixture,
  the CLI tests and the live re-imports are all checked without converting call sites,
  and record the baselines from the direct importer. Verify that the whole workspace's
  tests pass against them, and that a deliberate change to one import fails the test
  that imported it, naming the first difference.
- [x] 2.3 Add `crust-render/tests/import_parity.rs`: for each committed `samples/*.usd*`,
  `crust check --json` with the import's phases removed, compared with a baseline under
  `import_baselines/check/` (the same `CRUST_RECORD_BASELINES=1` records it), its
  digest checked by the `crust` process itself. The samples that need files outside the
  repository (`materialx_lion`, `materialx_teapot`, `materialx_showcase`) are listed and
  skipped where those files are missing; they take four to ten minutes each
  unoptimised, so a debug run compares them only with `CRUST_PARITY_EXTERNAL=1`, and a
  release run always does. The samples run side by side. Record the reports, and the image hashes of
  every import of the suite with `scripts/parity_images.sh record` (a release run with
  `CRUST_PARITY_IMAGES=1`), from the direct importer. The second import and its render
  are visible to the few tests that count asset requests or log lines, time a render or
  render a panicking material; they fail in that mode and are listed in
  `scripts/parity_images.expected`, and the script passes when no import differs and
  no other test fails. Verify that the test lists every sample, and that
  `scripts/parity_images.sh` passes.
- [x] 2.4 Add `scripts/import_parity.sh record|check <dir> <scene> [render flags]`.
  `record` renders the scene `-n` times at `-s 16 --indirect-clamp 0` with `--stats` and
  keeps the `crust check --json` report (timings removed), the EXR, and the peak memory
  and `Parse USD stage` time (the minimum over the runs) in `<dir>`. `check` does the
  same with the current build and compares: it exits non-zero on any report or image
  difference (`crust diff`), and prints both builds' memory and time with their ratios,
  without gating on them. Verify on `samples/cornellbox.usda` (record, then check exits
  0), and that a deliberately different render makes `check` exit non-zero.
- [x] 2.5 Record the large-scene baselines from the direct importer with
  `scripts/import_parity.sh record`: ALab (`samples/ALab/entry.usda -f 1004`) and Moana
  (`--camera /island/cam/shotCam`), plus Moho's `scripts/streaming-spike.sh --with-d`
  strategies (B) and (D) on Moana, on one machine. Also record `scripts/check_images.sh
  record` goldens and a `crust diagnostic --budget 2m` report on
  `samples/cornellbox.usda` (for group 14). Write the memory and time report in
  `openspec/specs/usd-scene-import/design.md` under a new "Index import" section, and
  keep the baseline directory for groups 9, 12 and 14. Verify that the section names
  the machine, the revs and the commands.

## 3. The index skeleton and the bridge (design D4, D5, D6, D7, D10)

- [x] 3.1 Create `usd_import/index/` (`mod.rs`, `walk.rs`, `bridge.rs`,
  `stage_facts.rs`), importing openusd through `moho::openusd` and
  `moho::openusd_schemas` (D2). Build the bridge chain (D6): per input
  `StageSceneIndex` → `Flattening` → `MaterialBindingResolving` →
  `RenderSettingsFlattening`, with no instancing filters, and light linking over it.
  Every stage is opened with Crust's `stage_builder()` and set to the import's time
  (none leaves the index's time unset, which is Moho's default time). Verify with a unit test that a chain over
  `samples/cornellbox.usda` lists its prims with their Hydra types.
- [x] 3.2 Single stage: `load_scene` wraps the `LoadAll` stage in one chain and walks
  it from the absolute root with the direct traversal's stack discipline, in place of
  `traverse_into`. Pruning reads the index: abstract and inactive prims are absent from
  it, and each prim's own `purpose` and `visibility` come from the generic attribute
  data, pruning the subtree where they are authored (Hydra's flattened purpose lets a
  child's authored purpose override an ancestor's `proxy`, which would change what
  renders), keeping the camera search in invisible subtrees. Every prim goes to the existing readers through
  the bridge (`StageSceneIndex::stage()` and the same path, under `EvalTimeScope`).
  Verify parity green on every single-stage sample and fixture.
- [x] 3.3 Streaming: build `chain::merged` over a `MergingSceneIndex` with the scene
  input (`LoadNone`, `set_include_unloaded_prims(true)`) and one chunk input at a time
  (masked to the chunk and the active pass prim). Add the input, walk from the root
  through that chunk's root only, remove it, drop its stage, and advance the epoch as
  today. The bridge resolves each path to the stage of the input that holds it. Verify:
  - parity green on every streamed sample and on a fixture with 5 top-level subtrees;
  - with `CRUST_STREAM_IMPORT=0`, a test that the streamed and single-stage imports are
    equal.
- [x] 3.4 Move the non-imaging stage facts (D7) behind `index/stage_facts.rs`: layers
  and layer stack, and invisible overrides, each with a comment naming the reason it is
  not imaging data. Read the time range's authored-ness (`check_time_range`) from the
  root's `__usdStageMetadata/{startTimeCode,endTimeCode}/authored` (M3), not from the
  stage. Verify parity green (`Scene::layers` and `Source` are in the digest), and that
  the time-range warnings match the baselines on a stage with and one without an
  authored range.
- [x] 3.5 Make `crust check`, `crust diagnostic` and the session's import go through
  `load_scene`'s index walk. Verify with a crust-render test that `check --json` on
  `samples/cornellbox.usda` equals its baseline, timings aside.

## 4. Settings, products, precomp, passes and camera

- [x] 4.1 Index reader for render settings: `renderSettings` from the scene input
  (camera, resolution, pixel aspect, conform policy, data window, motion and depth of
  field toggles, `renderingColorSpace`), and `crust:*` from `namespacedSettings`. The
  active settings come from the scene globals and the host's pass. Replaces
  `read_index`'s settings reads. Verify parity green, plus a test on `samples/aovs.usda`
  and the settings fixtures.
- [x] 4.2 Index reader for products and vars (`products`, `orderedVars` forwarded
  targets, every `UsdRenderSettingsBase` attribute a product overrides,
  `driver:parameters:*`, `ri:*`, `crust:aov:*`, `crust:filter*`), read from the generic
  data with the direct reader's own resolution of product over settings (UsdRender's
  compute-spec rule as Crust implements it, with its warnings), not from the flattened
  `renderProducts`, which carries resolved values without the authored-ness those
  warnings depend on. The precomp's settings relationships and display-filter inputs
  read from the index; the filters' shader ids, kernel assets and sample-filter graphs
  are shading networks and move with them (9.2). Verify parity green on `aovs.usda`,
  `aovs_lpe.usda`, `primvar_aovs.usda` and `precomp.usda`.
- [x] 4.3 Index reader for the active pass: `renderPass` and its `collections`
  (`matte`, `cameraVisibility`, and `prune`/`renderVisibility` for the existing warning
  that they are not honoured, M6). Membership is judged per chunk at the instance or
  instancer path, as today. Verify parity green on `crust-core/tests/render_passes.rs`
  and `holdouts.rs`, including a pass that applies all four collections.
- [x] 4.4 Index reader for cameras: the `camera` schema (projection, apertures and
  offsets, clipping, focus, shutter, exposure), `metersPerUnit` and its authored-ness
  from the root's `__usdStageMetadata/metersPerUnit/{value, authored}` (M3), and the
  dicing camera. Verify parity green on `camera_*.rs`, `aspect_conform.rs` and
  `orthographic` fixtures, with `metersPerUnit` authored and not.
- [x] 4.5 Move the settings-and-camera rebuild in `usd_import/live.rs` to the index
  readers (D8). Verify that `crust-core/tests/live.rs` passes, with the in-place versus
  re-import bitwise checks.

## 5. Transforms and motion

- [x] 5.1 Take world matrices from the `xform` (`resetXformStack` honoured), in place of
  `compose_with_parent`: the local `xform/matrix` of the input's stage scene index,
  composed by Crust as before, so the `f32`-from-`f64` bits stay (design, M9 note; the
  flattened matrix would round differently). Take `Timing` and spans from the
  `xform/matrix` values at the shutter offsets (`f32`, the listed exception) and the op
  attributes' `mightBeTimeVarying` (M9), plus `crust:motion:translate` from generic
  data. The walks still on the bridge (placement count, prototypes, a camera's
  ancestors, the live rebuilds) keep `compose_with_parent` until their families move. Verify parity green on `motionblur.usda`, `motionvector.usda`,
  `animation.usda` at `-f`, and the transform stack fixtures in `usd_scene.rs`.
- [x] 5.2 Move the "time-sampled motion not produced" reporting to the index's sample
  times. Verify that the warnings match the baselines in the motion fixtures.

## 6. Meshes

- [x] 6.1 Index reader for `mesh`: topology, holes, orientation, subdivision scheme and
  tags, `doubleSided`, and points and normals (`primvars:normals` precedence) read once
  as whole arrays. It feeds `MeshSource` and the existing bake and instance placement
  path. Verify parity green on `cornellbox.usda`, `displacement.usda`, the subdivision
  fixtures and `mesh_normals.rs`.
- [x] 6.2 Primvars: the render vars' primvars and materials' primvars with indices,
  interpolation, `elementSize` and colour space, de-indexed as the direct reader does.
  Constant primvars are read from the prim's own generic data, not the flattened
  (inherited) set (design, Risks). Verify parity green on `mesh_primvars.rs`,
  `primvar_aovs.usda` and `ptex_quads.usda`.
- [x] 6.3 `crust:subdivisionLevel`, `crust:subdivisionEdgeLength`,
  `crust:displacementBound` and `primvars:displacementbound:sphere` from generic data,
  and the adaptive level from the camera's projection. Verify parity green on the
  adaptive subdivision fixtures and `pxr_displace.usda`.
- [x] 6.4 Move the mesh parts of `usd_import/live.rs` (`mesh_source`, bake helpers) to
  the index reader. Verify that `live.rs` and the `live-geometry-edits` tests pass.

## 7. Implicit shapes and spheres

- [x] 7.1 Index readers for `sphere`, `cube`, `cone`, `cylinder`, `capsule` and `plane`
  (radius, size, height, axis, radiusTop/Bottom, width, length, doubleSided), tessellated
  by Crust's existing `implicit` code. Analytic spheres stay analytic. Verify parity
  green on `implicit_shapes.usda`, and that `geomutil_oracle` passes unchanged.
- [x] 7.2 Move the sphere rebuild in `live.rs` to the index reader. Verify that
  `live.rs` passes.

## 8. Curves

- [x] 8.1 Index readers for `basisCurves` and `nurbsCurves` (topology, widths, order,
  knots, ranges, point weights) and the `primvars:curve*` and `st` primvars that strand
  requests read. Verify parity green on `curves.usda`, `hair.usda`,
  `hair_variation.usda`, `curve_attributes.rs` and `hair.rs`.

## 9. Materials

- [x] 9.1 Binding: read `materialBindings` for the render purpose, then the
  all-purpose binding, from the material binding resolving filter. Check it against
  `winning_binding_target` with a test that runs both on `purpose_bindings.usda` and the
  binding fixtures, while the direct reader still exists. Verify parity green.
- [x] 9.2 Networks: `material` per render context (universal, `mtlx`, `ri`), and the
  precomp's display-filter shader ids, kernel assets and sample-filter graphs. Terminals,
  `nodeIdentifier`, parameters with colour space and resolved asset paths, and
  connections feed `preview` (`UsdPreviewSurface`, `UsdUVTexture`, primvar readers),
  `mtlx_network` (inline `ND_*`), `PxrDisneyBsdf` and `PxrDisplace`. `crust:openpbr` is
  read from the material's generic data. Verify parity green on every `materialx_*`,
  `openpbr_showcase.usda`, `ptex_quads.usda` and `mtlx_surfaces.rs`. Waits on Moho's M10
  (design, "Moho-side gaps found while migrating"); also verify
  `a_streamed_material_reads_a_shared_node_outside_its_subtree` in `usd_scene.rs`.
- [x] 9.3 `.mtlx` references: read the arc through `stage_facts` (D7) as
  `mtlx_reference` does, unless `mtlx-format-plugin` has landed, in which case the
  composed shader prims are read like any network. Verify parity green on
  `materialx_teapot.usda` and `materialx_lion.usda`.
- [x] 9.4 Materials outside a chunk: add the side input (D4), a `chain::input` over a
  stage masked to the paths `moho::streaming::unresolved` lists (M2), inserted weaker
  than the chunk and stronger than the scene input, grown and reopened under
  `OutsideMaterials`' rules (at most `MAX_SIDE_STAGE_OPENS` per chunk, design D4) until a
  round resolves nothing new, and removed after the last chunk. Verify:
  - parity on a fixture with `/World/Looks` beside four geometry chunks;
  - parity on a fixture whose material lives only in another chunk's payload;
  - parity on the Cornell box streamed.
- [x] 9.5 Provenance for materials (`material_reads`: arcs from `prim_stack`, targets
  from the generic `connections`), through `stage_facts` for the arcs. Move the material
  rebuild in `live.rs` to the index readers. Verify that `live.rs` passes, and parity
  green (provenance is in the digest).
- [x] 9.6 Run `scripts/import_parity.sh check` on ALab against its baseline. Verify that
  it exits 0, and record the run in the design record.

## 10. Lights and light links (design D9)

- [x] 10.1 Index readers for every light type through the `light` container
  (intensity, exposure, colour and temperature, normalize, the type's own inputs, dome
  texture and pole axis, shaping and IES), plus `crust:light:*` from generic data. They
  feed the existing `emit_*_light` builders. Verify parity green on `lights.rs`,
  `domelight.usda`, `dome_backdrop.usda`, `rectlight.usda`, `many_lights.usda`,
  `light_visibility.usda`, `hidden_lights.rs` and the IES samples.
- [x] 10.2 Light links: record receivers as today. Read each light's
  `collections/lightLink` and `collections/shadowLink` expressions from the merged data,
  and judge them after the last chunk without reading `categories` (D9; whether to read
  `categories` now that M1 has landed is the open question). Add agreement tests
  asserting that Moho's `categories` agree with Crust's answers: on a single stage
  (`chain::standard`), and on a streamed fixture whose light and receivers sit in
  different chunks (`chain::merged` over every input). Verify parity green on
  `light_linking.usda` and `light_linking.rs`, and on that streamed fixture.
- [x] 10.3 Move the light rebuild in `live.rs` to the index readers. Verify that
  `live.rs` passes.

## 11. Volumes

- [x] 11.1 Index reader for `crust:volume:*` regions from generic data, dispatched
  before geometry as today, and the material's volume terminal from the `material`
  network. Verify parity green on `fog.usda`, `materialx_volume.usda` and `medium.rs`.
  (The regions and their dispatch read the index; the volume terminal moved with the
  material networks, 9.2. `fog.usda`, `materialx_volume.usda` and `medium.rs` are green
  in the suite and the image run.)

## 12. Instancing and the standard chain

- [x] 12.1 Switch to `moho::chain::standard` (one stage), and to `chain::input` per
  input under `chain::merged` (streamed). Read `instancer` prims:
  - native instances: `instancerTopology` and `hydra:instanceTransforms`, with
    prototypes walked under their stable paths through the same readers;
  - point instancers: raw `protoIndices`, positions, orientations, scales and
    invisible/inactive ids from `__usdAttributes` where a warning depends on them, and
    `instancerTopology` otherwise.

  Key prototype caches on the stable prototype paths, with no epoch. Name prims inside
  a prototype, in warnings and provenance, by the instance's prototype joined with
  `primOrigin/scenePath` (M4). Verify parity green on `instancing.usda`,
  `nested_instancing.usda`, `PointInstancedMedCity.usd`, the Kitchen set variants and the
  `usd_scene.rs` instancing tests, with no path normalisation; a remaining difference is
  reported, not normalised. (Done: one chain per input, `moho::chain::input`, under
  `chain::standard`'s or `chain::merged`'s top; prototypes read at Moho's propagated
  paths, named by their USD prims, materials through Moho's re-rooted bindings (M11);
  caches on the propagated path, no epoch (D3c). A native instance is placed at its own
  world transform, which `hydra:instanceTransforms` holds too; a prototype reached
  through an instance proxy is still walked on the stage. The prototype numbers keep
  the listed normalisation: the USD names are openusd's `/__Prototype_N`.)
- [x] 12.2 Placement counts for adaptive subdivision and the bake-versus-instance
  decision from instancers, per top-level subtree. Instances judged at the instance or
  instancer path for links and passes. Verify that parity is green with adaptive
  subdivision on, on the instancing samples, streamed and not.
- [x] 12.3 Every index reader reads only the scene index or `stage_facts`: no bridged
  call remains. Verify with `grep` that `index/` names no `usd::Prim` outside
  `stage_facts.rs`, and that parity is green.
- [x] 12.4 Run `scripts/import_parity.sh check` on ALab and Moana against their
  baselines. Verify that both exit 0, and record the runs (and Moho's spike strategies
  (B) and (D) on the same machine) in the design record.

## 13. Delete the direct importer (design D3, D6)

- [x] 13.1 Delete the direct readers and the traversal they served (`traverse_into`,
  `count_placements`' stage walk, the per-family `usd::Prim` readers no index reader
  calls), the bridge (`index/bridge.rs`), `EvalTimeScope` and `time.rs`, and the
  prototype epoch (`ImportCaches::epoch`, `MaterialCache::epoch`). `listing.rs`
  (`crust ls`) is not an import and stays. Verify with `grep` that none of them remains,
  that `usd_import/` reads openusd only through `index/` and `stage_facts.rs` (and
  `listing.rs`), and that parity is green.
- [x] 13.2 Update the `CLAUDE.md` rules the deletion retires or changes: "The USD import
  is single-threaded and reads the time code from a thread-local (`EvalTimeScope`)", and
  the pair "Anything keyed on a prototype path is scoped by the stage epoch". Verify
  with `grep` that neither names a deleted item.

## 14. Hosts and large scenes

- [ ] 14.1 hdCrust: run `hydra/hdCrust/test/compare.sh`, `interaction.sh` and
  `usdc_roundtrip.sh`. Verify that all three pass.
- [x] 14.2 Run `crust diagnostic --budget 2m` on `samples/cornellbox.usda` and compare
  its report with the one recorded from the direct importer in group 2, timings aside,
  and run the MCP session tests (`crust-render` with feature `mcp`). Verify equal
  reports and green tests.
- [x] 14.3 Run `scripts/check_images.sh check` against the goldens recorded from the
  direct importer in group 2. Verify exit 0.
- [ ] 14.4 Run `scripts/import_parity.sh check` on ALab and Moana against their
  baselines on the final commit. Verify that both exit 0 and that the full CI is green,
  and record the runs in the design record.

## 15. Documentation and specs

- [x] 15.1 Update `openspec/specs/usd-scene-import/design.md`:
  - the import through Moho (layout, walk order, the bridge's history, stage facts);
  - the baselines and the parity runs;
  - Moho's gaps M1 to M7, closed in `scene-index-gaps`, with the Moho rev that closes
    them, and any parity exception that remains (none is planned).

  Verify that M1 to M7 are each named with what Crust uses from them.
- [x] 15.2 Update `docs/architecture.md`: Moho in the crate map and render flow, the
  `index/` module, and the new invariant that each family's index reader and its
  in-place rebuild change together. Update the `usd_import/mod.rs` module table to
  name each family's index reader. Verify with `grep` that each document names Moho
  and the rev rule.
- [x] 15.3 Add a page on the import to the site's architecture section (Crust reads
  USD through Moho, the scene index; how it streams), and update any site page that
  names the direct importer. Verify with `zola build`.
- [ ] 15.4 Drop the implementation status note on "One import route, through Moho", and
  run `openspec validate moho-import --strict`.
