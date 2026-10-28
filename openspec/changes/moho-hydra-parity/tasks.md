# Tasks

Every task ends green on `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace --no-fail-fast`. "Goldens hold" means
`scripts/check_images.sh check <dir>` passes against the goldens of task 1.2, or,
after task 8.3, against the re-recorded ones. One concern per commit.

## 1. Before anything moves

- [ ] 1.1 Archive `live-geometry-edits`, `live-session-host` and `live-session-time`
  (all tasks done), so that this change's `mcp-session` delta modifies their text
  (design, Migration Plan). Done when `openspec list` no longer shows them and
  `openspec validate moho-hydra-parity --strict` still passes.
- [ ] 1.2 Record goldens with main's binary: `scripts/check_images.sh record <dir>`.
  Also record `crust check --json` for every `samples/*.usda`, and the ALab and Moana
  `--stats` reports (peak memory, `Parse USD stage`) and 16 spp EXRs, for the
  exception review (task 8.1) and first-load comparison (task 9.3).

## 2. Remove the parity machinery (design D11)

- [ ] 2.1 Delete `crates/crust-core/tests/data/import_baselines/` (3 603 files),
  `crates/crust-core/src/parity.rs`, the `parity` feature in
  `crates/crust-core/Cargo.toml` (and its self dev-dependency line),
  `World::parity_digest` and its helpers in `crates/crust-core/src/rt_world.rs`,
  `Provenance::parity_digest` in `crates/crust-core/src/live.rs`, the
  `crate::parity::check_import` call in `usd_import/mod.rs` (`load_scene`), and
  `pub mod parity` in `crates/crust-core/src/lib.rs`. Done when no `parity` cfg or
  module remains (`grep -rn 'feature = "parity"' crates` is empty).
- [ ] 2.2 Remove `features = ["parity"]` from the crust-core dev-dependency in
  `crates/crust-render/Cargo.toml` and `crates/crust-hydra/Cargo.toml`; delete
  `crates/crust-render/tests/import_parity.rs`; drop `parity` from the `import` layer
  in `crates/crust-core/tests/layering.rs`; rewrite the doc comments that cite the
  baselines (`tests/asset_anchoring.rs`, `instanced_instancers.rs`,
  `xform_uncomposable.rs`, `usd_adaptive.rs`, `time_motion.rs` and its
  `.f32-shutter` note, `warnings.rs`).
- [ ] 2.3 Delete `scripts/import_parity.sh`, `scripts/parity_images.sh` and
  `scripts/parity_images.expected`; remove `CRUST_RECORD_BASELINES`,
  `CRUST_PARITY_IMAGES` and `CRUST_RECORD_F32_SHUTTER` wherever read; drop the
  `parity` caveat in `scripts/check_images.sh`'s comment. Goldens hold.

## 3. Remove streaming (design D6)

- [ ] 3.1 Delete `index::Streamed`, `input_chain`'s `include_unloaded` argument and
  the side input in `usd_import/index/mod.rs`; `stream_roots`, `MIN_STREAM_CHUNKS`
  and `traverse_stage`'s chunk branch in `usd_import/mod.rs`;
  `stage_facts::subtree_roots`; `ImportCtx::scene_input` and the payload-free index
  stage. Done when `traverse_stage` has one branch.
- [ ] 3.2 Delete `OutsideMaterials`, `material_stage`, `MAX_SIDE_STAGE_OPENS` and
  `MatSource::new`'s fallback `input_chain` in `usd_import/materials.rs`; light
  links' per-input opinions (`LightLinks::light_added`'s chunk/whole comparison) in
  `usd_import/light_links.rs`; the per-input walk in `index/walk.rs`.
- [ ] 3.3 Remove `CRUST_STREAM_IMPORT` from `crates/crust-core/src/config.rs`, its
  row in `docs/architecture.md` § Environment switches and its section in
  `site/content/docs/reference/environment-variables.md`. Delete
  `crates/crust-core/tests/streamed_materials.rs`,
  `crates/crust-core/tests/streamed_light_links.rs`,
  `usd_scene.rs::a_streamed_material_reads_a_shared_node_outside_its_subtree`, the
  streamed agreement tests in `light_links.rs`, and the streamed cases of
  `tests/asset_anchoring.rs`. Goldens hold.

## 4. Bump Moho to `510174c` and adapt (design D4, D7, D8)

- [ ] 4.1 Bump `moho` in the root `Cargo.toml` to
  `510174c04ade250303be224cbd2ce31fdfa69081`, regenerate `Cargo.lock`, and record the openusd APIs this
  change starts using (`Stage::apply_diff`, `usd::Diff`, `Edit`, `ApplyMode`) in the
  CLAUDE.md openusd note and the `usd-scene-import` design record's openusd section.
  Its own commit, with the minimal adaptation that compiles:
  - `index::Single` builds `chain::standard` plus `chain::renderer_filters(…,
    &[RendererFilter::light_linking(), RendererFilter::dependency_forwarding()])`;
  - the `MaterialNodeResolvingSceneIndex` top and `CollectionMembership` are gone;
  - render prim tokens `__usdRenderSettings`, `__usdRenderProduct` and
    `__usdRenderVar` are used;
  - the time range comes from `sceneGlobals`, `metersPerUnit` from the stage
    (`stage_facts`), and `__usdUpAxis` from the root.
  Done when `cargo tree -i openusd` shows one package, through `moho`.
- [ ] 4.2 Read material networks from `material/<context>` (`__all` not used for
  rendering, the named contexts are) in `usd_import/index/network.rs`,
  `materials.rs`, `preview.rs` and `mtlx_network.rs`: no `__materialNodes`, shaders
  are not prims, `colorSpace` is a token. Done when every `samples/materialx_*.usda`
  and `usdpreview_textured.usda` renders against its golden.
- [ ] 4.3 Instancing on Hydra's layout: native instancers at
  `…/UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer` with their
  `UsdNiPrototype`, point instancers through their `ForInstancer<hash>` copies, in
  `usd_import/instancing.rs` and `index/instancers.rs` (`native_prototype`,
  `instancer_prototypes`). Remove M12 (`ProtoRoot.outer`, `proto_roots`' `held`,
  `enclosing_instance`, `PrimData::with_outer`/`outer`) and the
  `PROPAGATED_ROOT` filters in `index/walk.rs`, `index/data.rs` and
  `instancing.rs::proto_children`. Invert the three M12 fixtures in
  `crates/crust-core/tests/instanced_instancers.rs`: the target places nothing.
  Keep `nested_instancing.usda`, `instancing.usda` and the Kitchen sets on their
  goldens.
- [ ] 4.4 Light linking from `categories`, `instanceCategories` and
  `light/lightLink` / `light/shadowLink` in `usd_import/light_links.rs`: delete
  `link_query`, `opinions`, `inside_instance` and the agreement tests; keep the
  link tables (`encode_shadows`, `demote`, `index_remap`). Retire the
  `light_link.*` codes the delegate can no longer observe from
  `crates/crust-core/src/warnings.rs` and `site/content/docs/reference/warnings.md`
  together. `light_linking.usda`, `hidden_lights.rs` and `light_linking.rs` pass.
- [ ] 4.5 Time: with no `-f`, leave Moho's time at 0. Add a test in
  `crates/crust-core/tests/time_motion.rs`: an attribute with `default = 1` and
  samples `{0: 5, 10: 7}` reads 5 without `-f` (spec "No frame given").
- [ ] 4.6 Purpose, visibility and pruning from Hydra's flattened `purpose` and
  `visibility` (`index/walk.rs`'s `prune`); an undefined `over` with a type is a
  prim of that type. Add the spec's "A child purpose under a proxy" and "An
  undefined camera" scenarios as tests in `crates/crust-core/tests/usd_scene.rs`.

## 5. Hydra-only reads and renderer attributes (design D4, D5)

- [ ] 5.1 List every `crust:*` attribute and relationship the samples
  (`samples/*.usda`, 42 files) and the `#usda` fixtures under `crates/*/tests`
  author, by family. Each family's commit in task 5.2 migrates its own: geometry
  and volume attributes to constant `primvars:crust:*`, `rel crust:*` settings to
  `string` paths, and `samples/precomp.usda`'s filters to `Material` prims with
  `outputs:crust:sampleFilter` / `outputs:crust:displayFilter`. That way the goldens
  hold at every commit.
- [ ] 5.2 Move the readers family by family off `PrimData`'s generic accessors
  (`value`, `custom_*`, `metadata`, `type_name`, `targets`, `connections`,
  `attribute_names`, `authored`) onto Hydra locators (design D4), one commit
  per family:
  1. settings and products (`namespacedSettings`);
  2. camera;
  3. xform (flattened, sampled over the shutter);
  4. mesh;
  5. shapes and strands;
  6. implicit shapes;
  7. lights (`light/<name>`);
  8. volumes (primvars);
  9. materials;
  10. passes (`collections`);
  11. precomp (`material/crust`).
  Goldens hold after each, except for differences listed for task 8.1.
- [ ] 5.3 Delete `PrimData`'s generic accessors and `moho::tokens::usd` uses outside
  `stage_facts.rs`; `stage_facts::time_range` reads `sceneGlobals`.
- [ ] 5.4 Add the `attribute.legacy_crust` warning
  (`crates/crust-core/src/warnings.rs` and
  `site/content/docs/reference/warnings.md`), raised by `crust check` from a
  host-side stage walk in `stage_facts.rs`, never by `crust render`. Add a test in
  `crates/crust-render/tests/check.rs` (spec "A legacy attribute").
- [ ] 5.5 Tests for "Renderer attributes travel as Hydra carries them": a ray mask
  primvar, an inherited subdivision level, a `string crust:dicingCamera`, a
  `crust:light:cameraVisible` light, a precomp `Material` filter.

## 6. The delegate (design D1 to D3)

- [ ] 6.1 Create `crates/crust-core/src/scene/hydra/` (`delegate.rs`,
  `factory.rs`, `prims/`, `commit.rs`), and add `hydra` to the `import` layer in
  `crates/crust-core/tests/layering.rs`. Move the family readers there with
  `git mv`, keeping `pub(super)` sharing and explicit `use` blocks (CLAUDE.md).
- [ ] 6.2 Add `crates/crust-core/tests/hydra_only.rs`: no file under
  `src/scene/hydra/` names `moho::stage`, `moho::openusd::usd`,
  `StageSceneIndex` or `moho::tokens::usd`. Host facts reach the delegate as
  `HostFacts`, filled in `usd_import/`.
- [ ] 6.3 Implement the factory and the managed prims (design D2), with
  `PrimTypePriority` as design D1 orders it. `dirty` records only. Each prim syncs at
  the commit.
- [ ] 6.4 Implement the commit (design D3): path-ordered ids, lights and volumes;
  the bake-or-instance decision and adaptive placement counts over the whole
  stage; the depth-first first-camera fallback; batch mode consuming the state.
  `load_scene` becomes host → delegate → one `update()` → commit; delete
  `index/walk.rs`, `visit_prim`, `count_placements` and `ImportCtx`'s walk state.
  Add a test that two notice orders (the prim types' priorities reversed) commit
  bitwise-equal scenes.
- [ ] 6.5 hdCrust: `hydra/hdCrust/sceneWriter.cpp` writes `crust:dicingCamera` as
  a `string` attribute instead of a relationship. `compare.sh`, `interaction.sh`
  and `usdc_roundtrip.sh` pass (`.github/workflows/hydra.yml`, or locally with
  `pixi run compare-sysroot` / `interaction-sysroot`).
- [ ] 6.6 Add the spec's "Missing data reads as its default" scenarios as tests (a
  native instancer without `xform` or `visibility`, a light without `xform`).
  Goldens hold.

## 7. The live session on the delegate (design D9)

- [ ] 7.1 Turn `layer_diff`'s result into a `usd::Diff` (`Create`, `SetField`,
  `EraseField`, `RemoveSpec`, parents before children, children removed before
  parents) in `crates/crust-render/src/session/delta.rs`, and replay it onto the
  held stage with `ApplyMode::ExactLayer`. On a replay error, fall back to `full`.
- [ ] 7.2 Keep the stage and the delegate in the session's state
  (`session/live.rs`, `session/scene.rs`). `apply` replays, then calls
  `apply_pending_updates` and `update()`, classifies from the batch
  (`hydra::commit::classify`, design D9's table), and commits in place or
  re-imports. Delete `classify`'s allow-list, `usd_import/live.rs`, and
  `crust_core::live`'s `Change`, `Update` and masked-stage rebuild, plus the
  `arc_sources` provenance. Keep `Provenance` for picks and hit names.
- [ ] 7.3 `set_time` sets the index's time and goes through the same table
  (`Imported::update` no longer refuses another time when the table accepts it).
- [ ] 7.4 Keep every bitwise pair in `crates/crust-core/tests/live.rs` and
  `crates/crust-render/src/session/live_tests.rs`, and add the spec's: a light with
  trivial collections added (`lights`), a light linked to one prim added
  (`lights`), a time change that moves only a camera.

## 8. Goldens and the parity exceptions (design D11)

- [ ] 8.1 Compare against task 1.2's goldens and reports. Explain every difference by
  a listed exception: Moana's coral, purpose, `over` prims, time 0, whole-stage
  adaptive counts, path-ordered ids. For the noise ones, show a 1/√N fall across
  16, 64 and 256 spp. A difference that is none of these is a bug: fixed in Crust,
  or reported to Moho.
- [ ] 8.2 Record the exceptions, with their measurements, in
  `openspec/specs/usd-scene-import/design.md` § "Index import" → "Behaviour changes
  and parity exceptions". Rewrite that section's layout, walk, stage facts, Moho's
  gaps (M10 and M12 retired, M1 and M2 gone with streaming) and "Baselines"
  (retired) for the delegate.
- [ ] 8.3 Re-record the goldens with `scripts/check_images.sh record`.

## 9. Measurements, reported and not gated (design D12, D13)

- [ ] 9.1 Add `crates/crust-render/examples/update_latency.rs` (feature
  `session`): the fixture generator (N meshes and a camera), the four edits, `--runs`
  and `--fixture N` or a stage path with `--camera` and `--linked`, the median and
  worst, the split and the class per edit. Add its recipe to
  `openspec/specs/cli/design.md`'s cookbook.
- [ ] 9.2 Run it on the fixture at 1 000 and 10 000 meshes (50 runs) and on Moana (10
  runs, under `systemd-run --scope -p MemoryMax=80G -p MemorySwapMax=0`). Record the
  method, the machine and the tables in `openspec/specs/mcp-session/design.md` §
  "Update latency", beside Moho's figures from Moho's `docs/update-latency.md`.
- [ ] 9.3 Re-measure first load on ALab and Moana (`crust render --stats -s 16`:
  peak memory, `Parse USD stage`, population, flush, commit). Record them in the
  `usd-scene-import` design record against S2's end (ALab 21.14 GiB / 57.1 s, Moana
  24.18 GiB / 2 171 s) and the direct importer's. Note what `MatSource`'s misses cost
  before and after.

## 10. Documentation and checks

- [ ] 10.1 `CLAUDE.md`:
  - "What this is": the importer is a Hydra 2.0 delegate over Moho.
  - The `usd-scene-import` row of the documentation table: no streaming.
  - The Rules: the readers read Hydra schemas only.
  - The Pairs, changed together with `docs/architecture.md` § Invariants: "Anything
    keyed on a prototype is keyed by its Hydra prototype path; names come from
    `primOrigin`", and "an MCP edit applied in place ↔ a full re-import" through the
    delegate's commit.
- [ ] 10.2 `docs/architecture.md`:
  - the crate table's `crust-core` row and the render flow (the delegate, no index
    stage, no side input);
  - § Invariants "Import cache keys" and "A family's index reader ↔ its in-place
    rebuild" (now: the managed prim ↔ the commit);
  - the `CRUST_SCENE_DELTAS` row;
  - the module map (`scene/hydra/`).
- [ ] 10.3 Site:
  - `usd/geometry.md`, `usd/volumes.md`, `usd/lights.md`, `usd/render-settings.md`,
    `usd/render-passes.md` (gap removed), `usd/aovs.md`: the `crust:*` forms and a
    migration table;
  - `architecture/usd-import.md`: the delegate, no streaming, the parity exceptions;
  - `reference/environment-variables.md` and `reference/warnings.md`.
  `zola build` in `site/` passes.
- [ ] 10.4 Run the three checks and `cargo deny --locked check`. Measure the net size
  with `git diff --shortstat main -- crates scripts` (baselines apart) and record it in
  this change's design "Size". Run `openspec validate moho-hydra-parity --strict`. At
  archive, delete `openspec/specs/import-parity/`, which this change empties.
- [ ] 10.5 Write the next change's proposal stub, `hdcrust-scene-index` (design D10),
  or note it in `hydra-render-delegate`'s Open Questions if the user prefers to fold
  it there.
