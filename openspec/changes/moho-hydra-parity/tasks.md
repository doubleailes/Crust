# Tasks

Every task ends green on `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace --no-fail-fast`. "Goldens hold" means
`scripts/check_images.sh check <dir>` passes against the goldens of task 1.2, or,
after task 10.3, against the re-recorded ones. One concern per commit.

## 1. Before anything moves

- [ ] 1.1 Archive the changes this one supersedes, with
  `openspec archive --skip-specs`: `live-geometry-edits`, `live-session-host`,
  `live-session-time`, `live-pick-and-camera-retarget` and `hydra-render-delegate`.
  The live changes' deltas target `mcp-session`, which this change retires.
  `hydra-render-delegate` is superseded by hdCrust's removal (design D10), and its open
  tasks (1.2, 3.6, 6.1, 7.1) are dropped. Record in each archived proposal what is
  superseded and what survives (design, Migration Plan). Archive `add-precomp-filters`
  first if its last task is done; otherwise note there that its authoring changes here.
- [ ] 1.2 Record goldens with main's binary (`scripts/check_images.sh record <dir>`),
  `crust check --json` for every `samples/*.usda`, and ALab's and Moana's `--stats`
  reports (peak memory, `Parse USD stage`) and 16 spp EXRs. They feed tasks 10.1 and
  11.3.

## 2. Remove the parity machinery (design D11)

- [ ] 2.1 Delete:
  - `crates/crust-core/tests/data/import_baselines/` (3 603 files);
  - `crates/crust-core/src/parity.rs` and `pub mod parity` in `lib.rs`;
  - the `parity` feature and its self dev-dependency in `crates/crust-core/Cargo.toml`;
  - `World::parity_digest` and its helpers in `rt_world.rs`;
  - `Provenance::parity_digest` in `live.rs`;
  - the `crate::parity::check_import` call in `usd_import/mod.rs`.
  Done when `grep -rn 'feature = "parity"' crates` is empty.
- [ ] 2.2 Remove `features = ["parity"]` from `crates/crust-render/Cargo.toml` and
  `crates/crust-hydra/Cargo.toml` (deleted whole in task 7.3). Delete `crates/crust-render/tests/import_parity.rs`.
  Drop `parity` from the `import` layer in `crates/crust-core/tests/layering.rs`.
  Rewrite the doc comments that cite baselines in:
  - `tests/asset_anchoring.rs`, `instanced_instancers.rs`, `xform_uncomposable.rs`;
  - `usd_adaptive.rs`, `time_motion.rs`, `warnings.rs`.
- [ ] 2.3 Delete `scripts/import_parity.sh`, `scripts/parity_images.sh` and
  `scripts/parity_images.expected`. Remove `CRUST_RECORD_BASELINES`,
  `CRUST_PARITY_IMAGES` and `CRUST_RECORD_F32_SHUTTER`, and the `parity` caveat in
  `scripts/check_images.sh`. Goldens hold.

## 3. Remove streaming (design D6)

- [ ] 3.1 Delete:
  - `index::Streamed`, `input_chain`'s `include_unloaded` and the side input
    (`usd_import/index/mod.rs`);
  - `stream_roots`, `MIN_STREAM_CHUNKS` and `traverse_stage`'s chunk branch
    (`usd_import/mod.rs`);
  - `stage_facts::subtree_roots`;
  - `ImportCtx::scene_input` and the payload-free index stage.
- [ ] 3.2 Delete:
  - `OutsideMaterials`, `material_stage`, `MAX_SIDE_STAGE_OPENS` and `MatSource::new`'s
    fallback `input_chain` (`usd_import/materials.rs`);
  - the per-input opinions in `LightLinks::light_added` (`light_links.rs`);
  - the per-input walk (`index/walk.rs`).
- [ ] 3.3 Remove `CRUST_STREAM_IMPORT` from `crates/crust-core/src/config.rs`, from
  `docs/architecture.md` § Environment switches and from
  `site/content/docs/reference/environment-variables.md`. Delete:
  - `tests/streamed_materials.rs` and `tests/streamed_light_links.rs`;
  - `usd_scene.rs::a_streamed_material_reads_a_shared_node_outside_its_subtree`;
  - the streamed agreement tests in `light_links.rs`;
  - the streamed cases in `asset_anchoring.rs`.
  Goldens hold.

## 4. Bump Moho to `510174c` and adapt (design D4, D7, D8)

- [ ] 4.1 Bump `moho` in the root `Cargo.toml` to
  `510174c04ade250303be224cbd2ce31fdfa69081` and regenerate `Cargo.lock`, in its own
  commit, with the minimal adaptation that compiles:
  - `chain::standard` plus `chain::renderer_filters(…,
    &[RendererFilter::light_linking(), RendererFilter::dependency_forwarding()])`;
  - no `MaterialNodeResolvingSceneIndex` and no `CollectionMembership`;
  - the `__usdRender*` tokens;
  - the time range from `sceneGlobals`, `metersPerUnit` from the stage, and
    `__usdUpAxis`.
  Done when `cargo tree -i openusd` shows one package, through `moho`.
- [ ] 4.2 Read material networks from `material/<context>` (no `__materialNodes`;
  shaders are not prims; `colorSpace` is a token) in `index/network.rs`,
  `materials.rs`, `preview.rs` and `mtlx_network.rs`. Done when every
  `samples/materialx_*.usda` and `usdpreview_textured.usda` holds its golden.
- [ ] 4.3 Instancing on Hydra's layout in `usd_import/instancing.rs` and
  `index/instancers.rs`:
  - native `UsdNiInstancer` / `UsdNiPrototype`, and point instancers' `ForInstancer`
    copies;
  - remove M12 (`ProtoRoot.outer`, `proto_roots`' `held`, `enclosing_instance`,
    `PrimData::with_outer`) and the `PROPAGATED_ROOT` filters in `walk.rs`, `data.rs`
    and `proto_children`;
  - invert the three M12 fixtures in `tests/instanced_instancers.rs`.
- [ ] 4.4 Light linking from `categories`, `instanceCategories` and
  `light/lightLink` / `light/shadowLink` (`light_links.rs`). Delete `link_query`,
  `opinions`, `inside_instance` and the agreement tests. Retire the unobservable
  `light_link.*` codes from `warnings.rs` and `reference/warnings.md` together.
- [ ] 4.5 Time 0 without `-f`. Add a test in `tests/time_motion.rs`: `default = 1`
  with samples `{0: 5, 10: 7}` reads 5.
- [ ] 4.6 Purpose, visibility and pruning from Hydra's flattened values. A typed
  undefined `over` is a prim of its type. Add tests in `tests/usd_scene.rs` for "A
  child purpose under a proxy" and "An undefined camera".

## 5. Hydra-only reads and renderer attributes (design D4, D5)

- [ ] 5.1 List every `crust:*` attribute and relationship that `samples/*.usda` (42
  files) and the `#usda` fixtures under `crates/*/tests` author, by family.
- [ ] 5.2 Move the readers off `PrimData`'s generic accessors onto Hydra locators, one
  commit per family. Each commit migrates its own samples and fixtures (geometry and
  volume attributes to constant `primvars:crust:*`, `rel crust:*` settings to `string`
  paths):
  1. settings and products (`namespacedSettings`);
  2. camera;
  3. xform (flattened, sampled);
  4. mesh;
  5. shapes and strands;
  6. implicit shapes;
  7. lights (`light/<name>`): `crust:rayMask`, `crust:light:*` and
     `primvars:ri:attributes:visibility:camera` stay attributes on lights and are not
     migrated. That covers `samples/light_visibility.usda`, `samples/motionblur.usda`
     and the light fixtures in `tests/hidden_lights.rs`, `light_linking.rs`,
     `render_passes.rs`, `usd_inline.rs` and `usd_scene.rs`. Only their geometry
     `crust:rayMask` moves to `primvars:crust:rayMask`. Add the `lighting` scenarios
     as tests in `tests/hidden_lights.rs`;
  8. volumes (primvars);
  9. materials;
  10. passes (`collections`);
  11. precomp: `samples/precomp.usda` and `crates/crust-render/tests/precomp.rs` move
      to `Material` filters with `outputs:crust:*` (`material/crust`).
  Goldens hold after each commit, except for the differences reviewed in task 10.1.
- [ ] 5.3 Delete `PrimData`'s generic accessors (`value`, `custom_*`, `metadata`,
  `type_name`, `targets`, `connections`, `attribute_names`, `authored`) and every
  `moho::tokens::usd` use outside `stage_facts.rs`.
- [ ] 5.4 Add tests for "Renderer attributes travel as Hydra carries them":
  - a ray mask primvar;
  - an inherited subdivision level;
  - a `string crust:dicingCamera`;
  - a `crust:light:cameraVisible` light;
  - a precomp `Material` filter.

## 6. `crust check`'s stage checks (design D5, D11)

- [ ] 6.1 Add three warnings to `crates/crust-core/src/warnings.rs` and
  `site/content/docs/reference/warnings.md`, raised by `crust check` only, from one
  host-side stage walk in `stage_facts.rs`:
  - `attribute.legacy_crust`: legacy `crust:*` attributes and relationships, naming
    the replacement;
  - `settings.path_across_arc`: a `string` path setting on a prim that comes through
    a reference or payload, naming the setting and the arc;
  - `time.default_and_samples`: an attribute authoring both a default and time
    samples.
- [ ] 6.2 Add tests in `crates/crust-render/tests/check.rs` for the spec scenarios "A
  legacy attribute", "A path setting through a reference" and "A default beside
  samples is reported". Also check that `crust render` does not walk: its phases list
  no stage check.

## 7. Remove `crust mcp` and hdCrust (design D14, D10)

- [ ] 7.1 Delete:
  - `crates/crust-render/src/mcp/` and `crates/crust-render/tests/mcp.rs`;
  - the `mcp` subcommand in `main.rs`;
  - the `mcp` feature and its dependencies (`rmcp`, `tokio`, `schemars`, `base64`; keep
    `serde` and `serde_json` only if the reports need them);
  - `mcp` from `default`.
  Regenerate `Cargo.lock` and update `deny.toml` if an exception served only these.
  Done when `crust mcp` is refused as a usage error (the `cli` scenario "No MCP
  server") and `cargo deny --locked check` is clean.
- [ ] 7.2 Delete `packaging/mcpb/` and the `crust mcp` smoke test and bundle steps in
  `.github/workflows/nightly.yml`. Delete `site/content/docs/help/claude-desktop.md`
  and its links.
- [ ] 7.3 Remove hdCrust (design D10):
  - delete `hydra/` (hdCrust, its tests, `hide_mtlx_stdlib.sh`, `pixi.toml`,
    `pixi.lock`);
  - delete `crates/crust-hydra/` and remove it from the workspace members;
  - delete `.github/workflows/hydra.yml`;
  - remove `[profile.hydra]` and its comments from `Cargo.toml`, rewording the
    `catch_unwind` rule to "outside render control";
  - remove the `hydra/` lines from `.gitignore` and the `crust-hydra` mentions from
    `crates/crust-render/Cargo.toml`;
  - regenerate `Cargo.lock`.
  Done when `git ls-files hydra crates/crust-hydra` is empty and
  `cargo deny --locked check` is clean.
- [ ] 7.4 Remove hdCrust's documentation:
  - `site/content/docs/help/usdview.md` and its links;
  - the Hydra section of `site/content/docs/architecture/limitations.md` and the
    Hydra mentions in `architecture/overview.md`;
  - the hdCrust recipe in `openspec/specs/cli/design.md`.
  Delete `openspec/specs/hydra-delegate/design.md`, and move the parts still true of
  the delegate (D1's terminal-index observation, D8's panic containment) to
  `render-delegate`'s design record. Keep the mentions of usdview and Storm as
  OpenUSD's reference behaviour. `zola build` passes.

## 8. The delegate's scene side and the batch host (design D1 to D3)

- [ ] 8.1 Create `crates/crust-core/src/scene/hydra/` (`delegate.rs`, `factory.rs`,
  `prims/`, `commit.rs`) and add `hydra` to the `import` layer in `tests/layering.rs`.
  Move the family readers there with `git mv`, keeping `pub(super)` sharing and
  explicit `use` blocks.
- [ ] 8.2 Add `crates/crust-core/tests/hydra_only.rs`. No file under
  `src/scene/hydra/` or `crust-render/src/delegate/` may name `moho::stage`,
  `moho::openusd::usd`, `StageSceneIndex` or `moho::tokens::usd`. Host facts arrive
  as `HostFacts`.
- [ ] 8.3 The factory and managed prims (D2), with `PrimTypePriority` in D1's order.
- [ ] 8.4 The commit (D3) and the class table (`hydra::commit::classify`, D9):
  - path-ordered ids;
  - whole-stage bake-or-instance and adaptive counts;
  - the depth-first first-camera fallback;
  - batch mode consuming the state.
  `load_scene` becomes the batch host: open the stage, `chain::standard`, one
  `HydraScene`, one `update()`. Delete `index/walk.rs`, `visit_prim`,
  `count_placements` and `ImportCtx`'s walk state. Add a test that two notice orders
  commit bitwise-equal scenes.
- [ ] 8.5 `crust ls` lists from the batch host's index by Hydra type, at `-f` or else
  index time 0 (the `cli` scenario "A default beside a sample at 0" as a test in
  `crates/crust-render/tests/ls_json.rs`), in `usd_import/listing.rs`.
  `ls_json.rs` passes with the `cli` spec's listing rules.
- [ ] 8.6 Add tests for "Missing data reads as its default": a native instancer
  without `xform` or `visibility`, and a light without `xform`. Goldens hold.

## 9. Render control replaces the session (design D9)

- [ ] 9.1 Create `crates/crust-render/src/delegate/` (feature `delegate`, replacing
  `session`). It holds render control around `HydraScene`:
  - `start`, `update`, `cancel`, `cancel_render`;
  - `render`, `generation`, `snapshot` (`render`, `time`, `final`);
  - `aov`, `pick`, `converged`, the outcome;
  - `Hooks` and panic containment;
  - kept `Assets`.
  Move them from `session/live.rs` and `session/scene.rs`, without the session thread.
- [ ] 9.2 Delete:
  - `session/layer.rs`, `sources.rs`, `delta.rs`, `scene.rs`, `live.rs`,
    `live_tests.rs` and `mod.rs`;
  - `usd_import/live.rs`;
  - `crust_core::live`'s `Change`, `Update`, masked-stage rebuild and `arc_sources`.
  Keep `Provenance` for `pick`. Keep `CRUST_SCENE_DELTAS` as "every update is
  `full`", with its row in `docs/architecture.md`.
- [ ] 9.3 Rewrite `crates/crust-core/tests/live.rs` as bitwise pairs of an in-place
  update against a fresh delegate, per class. Add the `render-delegate` scenarios:
  - a shader input (`materials`);
  - a light with trivial collections added, and a light linked to one prim added
    (`lights`);
  - a mesh's transform (`geometry`);
  - focal length under adaptive subdivision (`full`);
  - a shutter change on a motion-blurred scene (`full`), and exposure (`settings`);
  - each other `full` case of the `settings` class (dicing projection, subdivision
    level, working colour space, dome camera visibility, kept primvars, precomp);
  - a prim added (`full`);
  - a time change moving only the camera (`settings`).
  Add render-control tests: restart on update, `cancel_render` returning at once, a
  pick, agreement with `crust render`.
## 10. Goldens and the parity exceptions (design D11)

- [ ] 10.1 Compare against task 1.2's goldens and reports. Explain every difference
  by a listed exception: Moana's coral, purpose, `over` prims, time 0, whole-stage
  adaptive counts, path-ordered ids. Noise differences must fall as 1/√N at 16, 64 and
  256 spp. Anything else is a bug, fixed in Crust or reported to Moho.
- [ ] 10.2 Record the exceptions and their measurements in
  `openspec/specs/usd-scene-import/design.md` § "Index import". Rewrite its layout,
  walk, stage facts, Moho's gaps (M10 and M12 retired; M1 and M2 gone) and
  "Baselines" (retired) for the delegate.
- [ ] 10.3 Re-record the goldens.

## 11. Measurements, reported and not gated (design D12, D13)

- [ ] 11.1 Add `crates/crust-render/examples/update_latency.rs` (feature `delegate`):
  - the fixture generator;
  - the four edits, each authored on the stage, then `apply_pending_updates`, then
    `update()`, until `render()` moves;
  - `--runs`, `--fixture N` or a stage with `--camera` and `--linked`;
  - median, worst, split and class.
  Add the recipe to `openspec/specs/cli/design.md`'s cookbook.
- [ ] 11.2 Run it on the fixture at 1 000 and 10 000 meshes (50 runs) and on Moana (10
  runs, under `systemd-run --scope -p MemoryMax=80G -p MemorySwapMax=0`). Record the
  method, the machine and the tables in a new
  `openspec/specs/render-delegate/design.md` § "Update latency", beside Moho's figures.
- [ ] 11.3 Re-measure first load on ALab and Moana (`--stats -s 16`: peak memory,
  `Parse USD stage`, population, flush, commit). Record it in the `usd-scene-import`
  design record against S2's end and the direct importer.

## 12. Documentation and checks

- [ ] 12.1 `CLAUDE.md`. Update:
  - "What this is": a Hydra 2.0 delegate over Moho;
  - the commands: no `crust mcp`;
  - the workspace paragraph: `crust-render`'s library has `delegate`, not `session`
    or `mcp/`;
  - the documentation table: the `mcp-session` row becomes `render-delegate`, and the
    `hydra-delegate` row goes;
  - the workspace and `unsafe` rules, the commands and the CI paragraph: no
    `crust-hydra`, hdCrust or `hydra.yml`;
  - the Rules: Hydra schemas only.
  Change the Pairs together with `docs/architecture.md` § Invariants:
  - prototype keys are Hydra prototype paths, with names from `primOrigin`;
  - "an update applied in place ↔ a fresh delegate's commit" replaces the MCP pair.
- [ ] 12.2 `docs/architecture.md`. Update:
  - the crate table and the render flow (batch host, delegate, render control; an
    MCP session section removed);
  - § Invariants "Import cache keys" and "A family's index reader ↔ its in-place
    rebuild" (now: managed prim ↔ commit);
  - § Environment switches;
  - the module map.
- [ ] 12.3 Design records:
  - move what survives of `openspec/specs/mcp-session/design.md` (render control, kept
    assets, in-place classes and their history) into
    `openspec/specs/render-delegate/design.md`, and delete the rest at archive;
  - rewrite the MCP mentions in the `usd-scene-import`, `materials`, `render-passes`,
    `light-path-inspection` and `cli` specs (and the `cli` Purpose) to name a kept
    delegate.
  Site:
  - `usd/geometry.md`, `usd/volumes.md`, `usd/lights.md`, `usd/render-settings.md`,
    `usd/render-passes.md`, `usd/aovs.md`, with a migration table;
  - `architecture/usd-import.md`;
  - `reference/command-line.md` (no `mcp`), `reference/environment-variables.md`,
    `reference/warnings.md`.
  `zola build` in `site/` passes.
- [ ] 12.4 Run the three checks and `cargo deny --locked check`. Measure the net size
  with `git diff --shortstat main -- crates scripts packaging` (baselines apart) and
  record it in this change's design "Size". `openspec validate moho-hydra-parity
  --strict` passes. At archive, delete `openspec/specs/import-parity/` and
  `openspec/specs/mcp-session/`, which this change empties.
