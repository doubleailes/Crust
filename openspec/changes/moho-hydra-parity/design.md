## Context

See proposal.md for the motivation. The state this design builds on (crust-render
main at `2b4d88e8`, Moho main at `510174c`):

- **Crust's import** (`crates/crust-core/src/scene/usd_import/`, 25 500 lines) is a
  walk (`index/walk.rs`) over Moho's chain, dispatching each prim by Hydra type from
  `visit_prim` in `mod.rs`.
  - Its readers mostly read Moho's generic data through `PrimData`
    (`index/data.rs`: about 110 `value()` and 150 `custom_*()` calls across 20 files,
    keyed by USD attribute names).
  - It composes world transforms itself (`compose_index`, `xform.rs`).
  - It judges light-link collections itself (`light_links.rs`).
  - It streams by chunk (`index::Streamed`, `traverse_stage`'s chunk loop,
    `stream_roots`, `MIN_STREAM_CHUNKS`, `OutsideMaterials`, side stages,
    `MatSource`'s fallback `input_chain`).
  - It relies on M10 (`MaterialNodeResolvingSceneIndex`), on M12 (`ProtoRoot.outer`,
    `proto_roots`, `enclosing_instance`, `PrimData::with_outer`) and on Moho's old
    native layout (`PROPAGATED_ROOT` filters; the `"/ForInstancer"` skip in
    `materials.rs`' agreement test).
- **What Moho now offers:**
  - `chain::standard(stage_index)` and `chain::renderer_filters(index,
    &[RendererFilter::light_linking(), RendererFilter::dependency_forwarding()])`;
  - `PrimTypeNoticeBatchingSceneIndex::new(input, Some(priority))` with `flush()`;
  - `PrimManagingObserver::new(index, Some(factory))`, with
    `PrimFactory::create_prim(&Added, &PrimManagingObserver)` and
    `ManagedPrim::dirty(&Dirtied, &PrimManagingObserver)`. A managed prim is released
    by dropping it.
  - Its scene indices are `Rc` and thread-confined, and a new index is at time code
    0. `__usdAttributes` and `__usdRelationships` are Moho's one addition, which a
    Hydra host does not have.
- **The live session** (`crates/crust-render/src/session/`, 3 347 lines with its
  tests) has two parts.
  - **The layer plumbing.** `LiveSession` follows layer files on disk (`load`,
    `load_owned`, `apply`). `layer.rs`, `sources.rs` and `delta.rs` diff and
    classify. In-place rebuilds go through `crust_core::live` over masked stages
    (`usd_import/live.rs`).
  - **Render control.** The render thread, cancel and restart, `render`,
    `generation`, `snapshot`, `aov`, `pick`, `converged`, the outcome and the time.
  `live-geometry-edits`, `live-session-host`, `live-session-time` and
  `live-pick-and-camera-retarget` built on it.
- **`crust mcp`** (`crates/crust-render/src/mcp/`, 7 187 lines;
  `crates/crust-render/tests/mcp.rs`, 1 672 lines) serves that session to an MCP
  client. It brings in `rmcp`, `tokio`, `schemars` and `base64`, a bundle under
  `packaging/mcpb/`, and a nightly job.
- **hdCrust** (`hydra/hdCrust/`, change `hydra-render-delegate`, 25/29) observes
  Hydra's terminal scene index and writes what it sees as USD layers. `crust-hydra`'s
  `Session` (`crates/crust-hydra/src/session.rs`) wraps `LiveSession` to import them.
  This change removes both (D10).
- **Mantle** drives Crust through `LiveSession::load_owned` and `apply`.
- **The parity machinery** of S2:
  - 3 603 baseline files (74 155 lines, 23 MB);
  - `crust-core/src/parity.rs` and the `parity` feature;
  - `World::parity_digest` and `Provenance::parity_digest`;
  - `crust-render/tests/import_parity.rs`;
  - two scripts and an expected list.

## Goals / Non-Goals

**Goals:**
- A strict Hydra 2.0 delegate. It is given a scene index, owns no stage or file,
  updates in place from the dirty locators, and is polled through a render-control
  API. Its code runs unchanged over any implementation of Moho's `SceneIndex` trait
  (Moho now; a Hydra host re-architected later, out of scope, D10).
- Update latency first: an edit costs what it touches.
- A much smaller codebase ("Size").

**Non-Goals:**
- Any Hydra host integration. hdCrust is removed and a host will be re-architected
  later, out of scope (D10).
- An MCP server (Mantle's, later) and Mantle's integration (rebuilt on its side).
- Any change to the integrator, the kernel, materials evaluation or the image outputs.
- A chunked import of Crust's own (D6).
- Light filters, skinning, coordinate systems, draw modes: still unread.

## Decisions

### D1. The delegate, and the hosts that hand it an index

```
host: stage ─ StageSceneIndex ─ chain::standard ──┐
                                                  ▼
delegate: renderer_filters(light linking @4, dependency forwarding @1000)
          ─ PrimTypeNoticeBatchingSceneIndex(priority)
          ─ PrimManagingObserver(CrustPrimFactory) ─► managed prims ─► commit ─► Scene
          ─ render control (render thread, restart, snapshot, aov, pick)
```

- **`crates/crust-core/src/scene/hydra/` holds the delegate's scene side.**
  - `delegate.rs`: `HydraScene::new(index: SceneIndexHandle, options, facts)`, and
    `update()`, which flushes, commits and returns the class.
  - `factory.rs` and `prims/`: one managed prim kind per Hydra type family.
  - `commit.rs`: the commit and the class table (D9).
  - The family readers, which move from `usd_import/`: `mesh`, `shapes`, `implicit`,
    `strands`, `lights`, `camera`, `materials`, `preview`, `mtlx_network`,
    `settings`, `products`, `precomp`, `passes`, `volume`, `instancing`, `adaptive`,
    `assets`.
- **`crates/crust-render/src/delegate/` holds the host-facing delegate.** It wraps
  `HydraScene` with render control (D9), under feature `delegate`, which replaces
  `session`. It is what a long-lived host holds.
- **The batch host is `usd_import/`.** It opens the stage (`stage_builder`, the
  `mmap` resolver), builds the `StageSceneIndex` and `chain::standard`, and sets the
  time. It fills `HostFacts` from `stage_facts.rs`: layers, the `.mtlx` reference,
  arc sources for provenance, openusd's errors behind `xform.uncomposable`, and the
  `crust check`-only stage checks (D5, D11). `crust render`, `check`, `diagnostic` and
  `light-paths` update once and render. `crust ls` lists the index by Hydra type
  (`listing.rs` stops walking the stage).
- **Threads.** Moho's index is `Rc`, so the delegate's `update` runs on the host's
  thread, the one that owns the index. Renders run on render-control threads over
  `Arc<Renderer>`. The session thread that hid the `!Send` stage from arbitrary
  callers goes: the host owns its stage and index.
- **What the delegate may not name.** `scene/hydra/` and `crust-render/src/delegate/`
  name no `moho::stage`, no `moho::openusd::usd` and no `moho::tokens::usd`. A test
  `crates/crust-core/tests/hydra_only.rs`, shaped like `one_openusd.rs`, fails
  otherwise.
- **Priorities**: `material` → light types → `lightFilter` → geometry →
  `instancer` → `camera` → render settings, products, vars, pass. This is hdPrman's
  intent. The commit does not depend on it.
- **Alternatives rejected:**
  - *Keep the walk.* A walk is not what a Hydra host gives.
  - *Keep a session that owns the stage.* That is the layer model this change
    removes: the host owns the stage.

### D2. Managed prims record; the commit reads

`dirty()` records locators only. At the commit, stale prims sync: they read through
the observer's `scene_index()` and rebuild their translated state. The kinds are
`Mesh`, `Curves`, `Sphere`, `Implicit`, `Volume` (a prim with
`primvars:crust:volume:type`), `Light`, `Material`, `Camera`, `Instancer`, and
render settings, products, vars and passes. A removed prim is dropped. A prim
carrying `instancedBy` is a prototype part, placed only through its instancer.

### D3. The commit: deterministic and path-ordered

- **Ids.** Geometry ids, light-list order and volume order follow `sdf::Path` order.
  A commit is a function of the managed prims' state, so an in-place update equals a
  fresh delegate's commit by construction. Moho's merging promises no child order, so
  stage order is not available. Images move at noise level once (D11).
- **The first-camera fallback** keeps depth-first order, by one walk of `children()`
  made only when no camera is named.
- **Bake or instance.** The commit makes the `flush_meshes` decision over every mesh
  and instancer, and counts adaptive placements over the whole stage (accepted). The
  per-top-level-subtree count was streaming's. ALab's adaptive images may change.
- **Batch mode** consumes the managed state and releases the index and stage before
  rendering. A kept delegate keeps its state and its geometry inputs
  (`WorldBuilder::keep_inputs`, from `live-geometry-edits`).

### D4. Reading Hydra, not USD

The locators read:

- **Mesh:** `mesh/{topology, subdivisionScheme, subdivisionTags, doubleSided}`, and
  `primvars/<name>/{primvarValue, indexedPrimvarValue, indices, interpolation, role,
  colorSpace, elementSize}`.
- **Curves:** `basisCurves/topology` and `nurbsCurves`.
- **Implicit shapes:** `sphere`, `cube`, `cylinder`, `cone`, `capsule`, `plane`.
- **Every prim:** `xform/{matrix, resetXformStack}` (flattened, sampled over the
  shutter by `sample_times`), `visibility/visibility`, `purpose/purpose`,
  `materialBindings/<purpose>/path`, `categories`, `instancedBy`, and
  `primOrigin/scenePath` for names.
- **Material:** `material/<context>/{nodes, terminals, interface, config}`.
- **Light:** `light/<name>`, `light/lightLink`, `light/shadowLink`.
- **Camera:** `camera/*`.
- **Instancer:** `instancerTopology/*`, the `hydra:instance*` primvars and
  `instanceCategories`.
- **Render settings:** `renderSettings/{renderProducts, namespacedSettings,
  includedPurposes, materialBindingPurposes, renderingColorSpace, shutterInterval,
  active}`.
- **Render pass:** `renderPass/*` and the pass prim's `collections`.
- **The root:** `sceneGlobals/*` and `__usdUpAxis`.

Hydra's values are taken as they are, fallbacks included, so the authored-versus-
fallback rule goes. `metersPerUnit` is a host fact. Motion warnings read
`sample_times` over the shutter.

### D5. Renderer attributes: where each `crust:*` goes (accepted)

| today (from `__usdAttributes`) | from this change | Hydra carries it as |
|---|---|---|
| `crust:rayMask`, `crust:subdivisionLevel`, `crust:subdivisionEdgeLength` (on a prim), `crust:displacementBound`, `crust:motion:translate`, `crust:volume:*` | constant primvar `primvars:crust:<name>` | `primvars`, flattened |
| settings `crust:*` attributes | unchanged | `renderSettings/namespacedSettings` |
| product and var `crust:*` attributes | unchanged | their `namespacedSettings` |
| `rel crust:dicingCamera`, `crust:sampleFilters`, `crust:displayFilters`, `crust:filter` | `string` / `string[]` absolute paths | `namespacedSettings` |
| on a UsdLux light: `crust:rayMask`, `crust:light:cameraVisible`, `crust:light:lpeTag`, `primvars:ri:attributes:visibility:camera` | unchanged (attributes on the light, never primvars) | `light/<name>`: `LookupLightParamAttribute` answers any non-token name with the prim's attribute of that name (`primAdapter.cpp`; Moho's `stage/light.rs`) |
| precomp graphs (free `Shader` prims) | a `Material` with `outputs:crust:sampleFilter` / `outputs:crust:displayFilter` | `material/crust/…` |

- **Why primvars.** They are the Hydra delegates' convention (hdPrman
  `primvars:ri:attributes:*`, arnold-usd `primvars:arnold:*`, Karma
  `primvars:karma:object:*`). They reach any Hydra host with no plugin, and they
  inherit.
- **Why strings.** UsdImaging's `namespacedSettings` lists namespaced attributes and
  only four `ri:` relationships. A string is not remapped across a reference or
  payload, so `crust check` raises `settings.path_across_arc` for each path setting
  whose prim comes through one. The host walks the prim's composition arcs in
  `stage_facts.rs`, and the warning names the setting and the arc.
- **Why `Material`.** A free shader has no Hydra data.
- **Legacy authoring.** `crust check` raises `attribute.legacy_crust` from a
  host-side walk. `crust render` never walks.

### D6. One stage, no chunking

Streaming existed because composing Moana cost openusd 75.74 GiB. Moho's update
benchmark now composes Moana `LoadAll` at a 7.01 GiB peak, its chain included.
Crust's own structures dominate the peak (24.18 GiB at S2's end).

Two alternatives are rejected:
- *Payload batches loaded and unloaded through one stage.* Unloading removes prims,
  and a delegate releases what it built for removed prims.
- *Crust-managed masked stages.* They recreate what Moho deleted.

If Moana's re-measured peak (task 11.3) is too high, it is reported. The lever is the
batch mode's consumed state, not chunking.

### D7. Instancing on Hydra's layout

- **Native instancers** are at
  `…/UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer`, with their
  `UsdNiPrototype`. Point instancer prototypes are their `ForInstancer<hash>` copies,
  named by `instancerTopology/prototypes`.
- **Placement.** The `Instancer` managed prim places parts by
  `hydra:instanceTransforms` (native) or by translations, rotations and scales
  (point), masked by `mask`. An absent `xform` is the identity.
- **Nesting.** A nested instancer is a prototype part of its outer instancer.
  `MAX_INSTANCE_NESTING` counts `instancedBy` depth.
- **Keys.** Parts are keyed by their Hydra prototype prim path, and named from
  `primOrigin/scenePath`. That pair replaces "keyed by Moho's propagated prototype
  path, stable across a streamed import's stages".
- **Moana's coral.** With M12 gone, a point instancer target inside a native instance
  is empty (UsdImaging 2.0), and isCoral's coral renders nothing (accepted). The three
  M12 fixtures in `instanced_instancers.rs` invert.
- **Flattened data on an instancer.** A native instancer may or may not carry it
  (Moho's parity check: "Paused" on main, "exempted" on `fix/hydra-parity-check`,
  PR #21). Missing means default reads both the same.

### D8. Light linking from categories

Lights read `light/lightLink` and `light/shadowLink`. Receivers read `categories`,
or their instancer's `instanceCategories` or `categories`. The commit builds Crust's
link tables (`encode_shadows`, `demote` and `index_remap` stay). Crust's own
collection judging goes (`link_query`, `opinions`, `inside_instance`, the agreement
tests), and so do the `light_link.*` warnings whose cause the delegate cannot
observe.

### D9. Updates in place, and render control

- **The class table.** `update()` decides from the batch: the prims added and
  removed, by type, and the dirtied locators. The table is in
  `hydra::commit::classify`, beside the commit it predicts:
  - `settings`: render settings, products or vars, or the render camera's `camera` or
    `xform`, unless the edit moves what geometry, lights or the world were built
    from. These are today's settings-rebuild refusals (`usd_import/live.rs`
    `settings_update`, the `ShutterKey` in `crust_core::live`), kept as the table's
    `full` cases:
    - the dicing projection;
    - the subdivision level or the working colour space;
    - the shutter (`shutter:open`/`close`, `shutterInterval`,
      `disableMotionBlur` where placements were posed at
      the frame);
    - the dome's camera visibility;
    - the products' kept primvars;
    - a precomp.
    A camera retarget is a `renderProducts` change, so `settings` under the same
    guards and when the new camera keeps the shutter
    (`live-pick-and-camera-retarget`'s retarget, reached here).
  - `materials`: a used material's `material`, when its class stays.
  - `lights`: a light's `light` or `material`; a light added or removed; a distant
    or dome light's `xform`; `categories` or `instanceCategories` alone.
  - `geometry`: a mesh's `xform` or `primvars/points|normals`, `mesh/*` under
    `live-geometry-edits` D4's conditions, and an area light's `xform` or shape.
  - `full`: anything else. A fresh `HydraScene` is built over the same index.
- **In-place commit and its pair.** In place, the commit runs over the updated managed
  prims; `full` runs over fresh ones. Their equality is the bitwise pair, tested per
  class (`crates/crust-core/tests/live.rs`, rewritten against a delegate).
  `Signature` (`rt_world.rs`) still guards what the world baked from a material. A
  composed edit reaches every composed copy through the stage's own notices, so
  `live-geometry-edits` D6's `arc_sources` goes. `CRUST_SCENE_DELTAS=0` makes every
  update `full`.
- **Time.** The host calls `set_time` on its `StageSceneIndex`. The dirtied locators
  go through the same table.
- **Render control** (`crates/crust-render/src/delegate/`). This is `LiveSession`'s
  part 2, moved and reshaped:
  - `start(spp, region)`;
  - `update()`, which cancels and joins a running render, commits, and restarts when
    the scene changed;
  - `cancel` and `cancel_render` (one atomic store);
  - `render`, `generation`, `snapshot` (with `Snapshot::render`, `time`, `final`);
  - `aov`, `pick` (`Provenance`, `name_hit`), `converged`, the outcome;
  - `Hooks` (`on_thread`, `on_warnings`) and panic containment, as
    `live-session-host` D1 lifted them.
  Kept assets (`Assets`, invalidated by length and modification time) move here from
  `session/scene.rs`. This is the analogue of an `HdRenderPass` with its
  `HdRenderBuffer`s.
- **What goes.** `session/layer.rs`, `sources.rs`, `delta.rs` (`layer_diff`,
  `classify`, `Applied` moves to the class table), `scene.rs`'s `Imported::update`,
  `live.rs`'s owned layers, `load`, `load_owned`, `apply`, `set_time` and the
  session thread, `usd_import/live.rs`, and `crust_core::live`'s `Change`, `Update`
  and masked-stage rebuild. The `usd::Diff` replay planned in the first draft of this
  change is dropped: Crust never follows layers.

### D10. hdCrust is removed; a Hydra host comes later

By the user's decision, hdCrust is dropped in this change rather than bridged. It is
the C++ Hydra plugin that wrote what Hydra presented as USD layers for `crust-hydra`
to import through `LiveSession`. A Hydra host integration will be re-architected
later, out of scope.

- **Removed** (inventory of main at `d4ee13ad`):
  - `hydra/`: hdCrust's 17 files (C++, CMake, `plugInfo.json`); its tests
    `compare.sh`, `interaction.sh`, `usdc_roundtrip.sh`, `crust_sysroot.sh` and
    `hdCrustTest.cpp`; `hide_mtlx_stdlib.sh`; `pixi.toml`; `pixi.lock`. That is
    4 333 lines, beside 4 306 in the lock.
  - `crates/crust-hydra/`: 8 files, 1 729 lines (the C ABI, `include/crust_hydra.h`,
    the session wrapper, its tests). It took the workspace's only other audited
    `unsafe` besides crust-core's and crust-jit's.
  - `.github/workflows/hydra.yml` (128 lines).
  - What only they need:
    - `[profile.hydra]` and its comments in `Cargo.toml`. The rule "no
      `catch_unwind` outside `crust-hydra`" becomes "outside render control";
    - the `hydra/` lines in `.gitignore`;
    - the `crust-hydra` mentions in `crates/crust-render/Cargo.toml`;
    - the hdCrust recipe in `openspec/specs/cli/design.md`'s cookbook;
    - `site/content/docs/help/usdview.md`, the Hydra section of
      `architecture/limitations.md`, and the Hydra mentions in
      `architecture/overview.md`;
    - `CLAUDE.md`'s workspace paragraph, `unsafe` rule, commands, documentation
      row and CI paragraph;
    - `docs/architecture.md`'s crate table and seams;
    - `openspec/specs/hydra-delegate/design.md`.
- **Kept:** mentions of usdview and Storm as the reference for how USD frames or
  tessellates something (`camera.rs`, `settings.rs`, `samples/implicit_shapes.usda`,
  `scripts/geomutil_oracle.py`, `usd/geometry.md`, `usd/render-settings.md`). They
  describe OpenUSD's behaviour, not hdCrust.
- **What a later host sits on.** The delegate's shape is unchanged: a host hands
  `HydraScene` a `SceneIndexHandle`, and `scene/hydra/` names no stage
  (`hydra_only.rs`). An FFI `SceneIndex` over a C++ `HdSceneIndexBase`, or any other
  host, can be built on it without touching the delegate.
- **Panics.** Render control still contains a panic where the host's build unwinds.
  The CLI's release profile aborts, as before. A host that wants containment (Mantle)
  sets `panic = "unwind"` in its own workspace.

### D11. The parity machinery goes; the goldens are the guard

The order of work, each step with its guard:
1. Record goldens and `crust check` reports on main.
2. Remove the parity machinery. No image changes.
3. Remove streaming. It is equal to no streaming by its spec.
4. Bump Moho and move the readers. Each image difference must be explained by a
   listed exception:
   - Moana's coral, purpose, `over` prims, time 0, whole-stage adaptive counts;
   - path-ordered ids, a noise difference that must fall as 1/√N at 16, 64 and 256
     spp.
   Then re-record.
5. The delegate replaces the walk. The goldens hold bitwise.

`crust check` also reports `time.default_and_samples` for each attribute with both a
default and time samples (accepted). It comes from a host-side walk, so no render
pays for it.

### D12. The update-latency benchmark

- **Where.** `crates/crust-render/examples/update_latency.rs` (feature `delegate`), a
  host over Moho: the stage, `StageSceneIndex`, `chain::standard`, and a delegate
  with render control at a small resolution.
- **One run:**
  1. author the edit on the stage (openusd);
  2. call `apply_pending_updates`;
  3. call the delegate's `update()`, which flushes, commits and restarts;
  4. stop the clock when `render()` has moved.
- **Reported.** Median (the mean of the two middle runs for even N) and worst over N
  runs. The split: authoring, Moho (`apply_pending_updates` and flush), commit,
  restart. Also the class.
- **Fixture and stage.** The fixture is Moho's (a camera with a translate op, N
  meshes, a linked light including `/world/a`). Moana uses `/island/cam/shotCam` and
  Moho's linked prim, so the figures continue Moho's `docs/update-latency.md`.
- **Recorded** in a new `openspec/specs/render-delegate/design.md` § "Update
  latency". Reported, not gated.

### D13. First load, re-measured

ALab and Moana are run with `crust render --stats -s 16`: peak memory, `Parse USD
stage`, population, flush, commit. The figures go into the `usd-scene-import` design
record against S2's end (ALab 21.14 GiB / 57.1 s, Moana 24.18 GiB / 2 171 s) and the
direct importer's. `MatSource`'s chains rebuilt on misses disappear with M10 and the
side stages. Reported, not gated.

### D14. `crust mcp` leaves Crust

The MCP server moves to Mantle, where an agent edits Mantle's stage and Mantle's
viewport renders through a Hydra scene index. It is a follow-up in Mantle, not
planned here. Deleted from Crust:
- `crates/crust-render/src/mcp/` and `crates/crust-render/tests/mcp.rs`;
- the `mcp` feature and its dependencies (`rmcp`, `tokio`, `schemars`, `base64`;
  `serde` and `serde_json` stay if the reports use them);
- `packaging/mcpb/`, the nightly `crust mcp` smoke test and bundle step;
- `site/content/docs/help/claude-desktop.md`;
- the `mcp-session` capability and its design record. Its surviving parts (render
  control, assets kept, the in-place classes) move to `render-delegate`.
The placement tools (`mcp/place.rs`, drop, align and scatter) go with it, and
`live-pick-and-camera-retarget`'s `probe` sharing reduces to `pick`.

## Size

Estimated from the files this change deletes or rewrites. Task 12.4 measures it with
`git diff --shortstat main -- crates scripts packaging`.

| | lines |
|---|---|
| parity machinery | −1 250 |
| streaming | −1 100 |
| the walk, `PrimData`'s generic accessors, M12, the old layout filters | −900 |
| `light_links.rs` judging collections | −600 |
| `xform.rs` composition | −500 |
| masked-stage rebuilds (`usd_import/live.rs`, `crust_core::live`'s `Change`/`Update`, arc sources) | −1 000 |
| `materials.rs`, `instancing.rs`, `passes.rs`, `listing.rs` on Hydra's data | −1 000 |
| `crust mcp` (`src/mcp/` 7 187, `tests/mcp.rs` 1 672) | −8 860 |
| session plumbing (`session/` 3 347 with `live_tests.rs`) | −3 350 |
| `crust-core/tests/live.rs` rewritten against a delegate | −500 |
| hdCrust (`hydra/`, without `pixi.lock`) | −4 333 |
| `crust-hydra` (all of it) | −1 729 |
| `.github/workflows/hydra.yml` | −128 |
| removed | **≈ −25 250** |
| delegate scene side (factory, managed prims, commit, class table, `HostFacts`) | +1 900 |
| render control (`crust-render/src/delegate/`) and its tests | +1 100 |
| `crust check` stage checks (legacy `crust:*`, path across an arc, default and samples) | +250 |
| update-latency example | +400 |
| new tests (Hydra-only, categories, time 0, instancing layout, in-place pairs) | +900 |
| added | **≈ +4 550** |
| **net, code** | **≈ −20 700 (±2 500)** |
| `hydra/pixi.lock` | −4 306 |
| baselines (3 603 files, 23 MB) | **−74 155** |

S2 added about +4 700 lines of code net. This change removes over four times that,
and all of S2's baselines. The design records also lose
`openspec/specs/hydra-delegate/design.md` (567 lines) and most of
`openspec/specs/mcp-session/design.md` (758 lines). The lock file also loses `rmcp`, `tokio` and their trees.

## Risks / Trade-offs

- **[Breaking scenes]** Geometry `crust:*`, `rel crust:*` settings, precomp shaders and
  default values beside samples change meaning.
  → `crust check` names each case; samples, fixtures and the site migrate.
- **[Breaking hosts]** Mantle and any `LiveSession` user stop compiling; `crust mcp`
  users lose the server.
  → Accepted by the user. Mantle rebuilds on its side and gains the MCP server later.
- **[Images move once]** → Explained against the listed exceptions, noise by 1/√N,
  before re-recording (D11).
- **[Moana memory without streaming]** → Measured and reported (D13).
- **[The class table under- or over-classifies]** → Every in-place class has a bitwise
  pair test against a fresh delegate. Unknown changes are `full`.
- **[No Hydra host]** usdview and Solaris users lose Crust as a renderer.
  → Accepted by the user. A Hydra host integration will be re-architected later, out
  of scope, on the render-delegate shape (D10).
- **[The change is large]** → Its phases each end green. The MCP removal (task 7) can
  land early as its own commit, since nothing else depends on it.

## Migration Plan

- **Order.**
  - The Moho bump is its own commit (Moho, then crust-render; Mantle rebuilds on its
    side).
  - Tasks land in order, one concern per commit.
  - Rollback is reverting the merge.
- **Unarchived changes this supersedes:**
  - `live-session-host` (complete). Superseded: `LiveSession` as a layer follower,
    `load_owned`, owned layers, classifying their union. Survives into
    `render-delegate`: `Hooks`, panic containment, `cancel_render`,
    `Snapshot::render`.
  - `live-session-time` (complete). Superseded: `LiveSession::set_time` and a time
    change as `full`. Survives: `Snapshot::time`, and the time is now the host's
    index time, applied in place when the table allows (its follow-up, reached).
  - `live-geometry-edits` (complete). Superseded: the layer classifier's `geometry`
    rules and `arc_sources` (D6). Survives: the `geometry` class and its D4
    conditions, `keep_inputs`, `commit_ref`, `Provenance::meshes`/`spheres`, and
    D1's rebuild-never-refit.
  - `live-pick-and-camera-retarget` (9/10). `pick` survives in render control. The
    camera retarget becomes a `settings` update from `renderProducts`. Its
    `LiveSession` and `probe` parts are superseded.
  Archive these four with `openspec archive --skip-specs` before this change: their
  deltas target `mcp-session`, which this change retires. What survives is specified
  anew in `render-delegate`.
- **`add-precomp-filters` (21/22).** Its sample and display filters become `Material`
  prims with `outputs:crust:sampleFilter` / `outputs:crust:displayFilter`, and its
  `rel crust:sampleFilters`, `crust:displayFilters` and `crust:filter` become string
  paths. Archive it first, and this change's task 5.2 migrates `samples/precomp.usda`
  and its tests. Otherwise its remaining task and its spec text must adopt the new
  authoring before it archives.
- **`payload-and-purpose-view` (0/32).** Its tasks naming `CRUST_STREAM_IMPORT` and the
  MCP session are stale and need rewriting before it starts.
- **`hydra-render-delegate` (25/29).** It is superseded: hdCrust and `crust-hydra` are
  removed (D10). Task 1.1 archives it with `openspec archive --skip-specs`, and its
  archived proposal says it was superseded by `moho-hydra-parity`. Its open tasks
  (1.2, 3.6, 6.1, 7.1) are dropped. Its spec never reached `openspec/specs`, which
  holds only its design record, `hydra-delegate/design.md`, deleted here. A Hydra host
  integration will be re-architected later, out of scope.
- **Mantle.** Its `mantle-crust` integration over `LiveSession` breaks with this
  change and is rebuilt on Mantle's side over a Hydra viewport scene index. The MCP
  server becomes a Mantle feature. Neither is planned here.

## Open Questions

- Whether Moho should expose a `metersPerUnit` that a Hydra host also has. A later
  Hydra host would need it from Hydra. It does not change this change's host fact.

## Implementation notes

Minor deviations met while implementing, recorded as they were made.

- **Test builds without debug info.** The workspace's debug test build with full debug
  info outgrew the machine's free disk (30 GB). Local runs of the three checks used
  `CARGO_PROFILE_DEV_DEBUG=0`; the code and the profiles are unchanged, so CI is not.
- **Streaming removed (task 3): two goldens move at noise level.** The new binary is
  bit-identical to main's single-stage import (`CRUST_STREAM_IMPORT=0`) on every sample.
  `mass_spike` and `purpose_bindings` streamed on main (at least four top-level
  subtrees), and a chunked walk visits subtrees in another order than one walk of the
  stage, so geometry and light ids differ. Against main's streamed render, at
  `--indirect-clamp 0 --variance-threshold 0` (adaptive stopping off, so every pixel takes
  the sample count), the beauty's relmse falls as noise:

  | sample | 16 spp | 64 spp | 256 spp |
  |---|---|---|---|
  | `mass_spike` | 7.99e-5 | 1.69e-5 | 3.73e-6 |
  | `purpose_bindings` | 2.40e-4 | 3.82e-5 | 6.45e-6 |

  The other 45 goldens are bit-identical.
- **Whole-stage placement counts arrive with streaming's removal.** Task 3.1 deletes
  `stage_facts::subtree_roots`, the unit adaptive subdivision counted native placements
  in, so placements are counted over the whole stage from task 3 on rather than from the
  commit of task 8.4 (D3). `usd_adaptive.rs`'s per-subtree test inverts
  (`placements_are_counted_over_the_whole_stage`). No sample's golden moves for it.
- **Tests that pinned the streamed order or its gaps** were updated rather than
  deleted where their subject survives: the first-seen order of `geom.unsupported_type`
  and `material.fallback_default` is now the single walk's; a camera in a payload is read
  ahead of the walk on the one stage, so `camera.shutter_mismatch` is not raised for it;
  a render pass collection included from another subtree now contributes.
- **Task 7 landed before task 4**, as Risks allows ("The MCP removal can land early"),
  so the Moho bump had no MCP server to adapt.
- **The Moho bump (task 4.1) is green from task 4.5, not on its own.** The bump is its
  own commit, and it breaks at once what tasks 4.2 to 4.5 fix family by family. The 4.1
  commit compiles, passes `fmt` and `clippy`, and leaves 13 tests red, each named for the
  task that turns it green: material networks (4.2: `displacement_sample`'s two,
  `usd_displacement`'s `a_string_info_id_is_read` and `only_meshes_open_displacement_maps`,
  `usd_scene`'s four texture and Ptex tests), instancing (4.3:
  `a_point_instancer_targeting_prims_inside_an_instance_places_them`, one of the three
  M12 fixtures that invert), and time code 0 (4.5: `usd_listing`'s
  `light_records_are_the_authored_inputs_at_the_time_code`, `usd_scene`'s
  `animated_stage_is_evaluated_at_the_requested_frame`).
- **Two other adaptations land in 4.1, because Moho removed what they used.** A render
  pass's collections are judged by Hydra's complete membership expression with Hydra's
  predicates (`CollectionMembership` is gone), so a USD-only predicate (`isa`) matches
  nothing (the listed collections exception; `render_passes.rs` tests both). And a free
  `Shader` carries no `__usdPrimInfo` (UsdImaging parity), so until precomp filters move to
  `Material` prims (task 5.2.11) a prim whose generic data declares
  `info:implementationSource` reads as a `Shader`.
- **A string-typed `info:id` names no shader (task 4.2).** Moho's network gives a node
  the identifier `UsdShadeShader::GetShaderId` reads, a token, so a `uniform string
  info:id`, which the old reader accepted, now leaves its node unidentified, as in
  UsdImaging. `usd_displacement.rs`'s test inverts
  (`a_string_info_id_is_not_an_identifier`). No sample authors one.
- **A texture file's colour space is Hydra's consolidated one (task 4.2).**
  `inputs:sourceColorSpace` is no parameter: UsdImaging folds it into `inputs:file`'s
  `colorSpace` (a non-empty `sourceColorSpace` wins, else the attribute's computed colour
  space). Where both are authored, `sourceColorSpace` now wins over the file's own
  `colorSpace` metadatum. Every material golden holds.
- **An area light's emitter carries no link category (task 4.4).** Hydra gives
  `categories` to geometry types only, and a light is none, so no restricting collection
  includes an emitter: a restricted light neither lights another light's source nor is
  shadowed by it, where the old reader judged the light's path like any prim's. Spec
  "Light collection membership" (a receiver is lit when the light's id is empty or among
  its categories) says the same. `light_linking.rs`'s hidden-source and overflow tests
  follow (the emitters form one more occluder class). No sample's golden moves.
- **Task 1.2's recordings live outside the repository**, as S2's did: the 47 goldens, 45
  `crust check --json` reports and ALab's and Moana's `--stats` reports and 16 spp EXRs,
  rendered by main's binary (`2f97d374`). Main's first load: ALab (`-f 1004`, the
  `renderCam`) `Parse USD stage` 58.1 s, peak 21.14 GiB; Moana (`shotCam`) 2 118 s, peak
  23.87 GiB, both while builds ran beside them.
- **Hydra's own renderer settings are not stage data (task 5.2.1).** UsdImaging's
  `namespacedSettings` lists namespaced attributes only, so an unnamespaced
  `domeLightCameraVisibility` or `enableExposureCompensation` on the settings prim, which
  the old reader also read (hdEmbree's names), reaches no Hydra delegate: in Hydra they are
  settings the application hands the delegate. Crust reads the `crust:` names only; the
  spec's "(and `crust:domeLightCameraVisibility`)" reads as "the `crust:` name". Two test
  fixtures move to the `crust:` names. No sample authors them.
- **Render settings fallbacks are Hydra's (task 5.2.1).** A settings prim that authors
  no `resolution` renders at UsdRender's fallback, `(2048, 1080)`, as UsdImaging gives
  it, not at Crust's `(640, 360)`, which applies only without a settings prim. A
  render var without `dataType` is `color3f`, so a variance var must author `float`
  (`usd_products.rs`). A product's refused `aspectRatioConformPolicy` reads as the
  default policy, not the settings' (Hydra flattens the product's own opinion). Every
  sample authors its resolution; no golden moves.
- **Targets that are no render product or var are dropped by Hydra (task 5.2.1).**
  The flattening keeps only real products and vars, so `product.not_a_render_product`
  and `aov.not_a_render_var` can no longer be raised; they are retired with their rows.
- **`crust:filter` on a var moves with the products (task 5.2.1)**, as it is a var's
  namespaced setting; the settings' filter lists move with the precomp (5.2.11).
- **The camera reads Hydra's lens values (task 5.2.2).** UsdImaging gives `focalLength`,
  the apertures and their offsets in tenths of a scene unit; Crust divides by 0.1 to get
  millimetres back, which is exact for most authored values but not all (42 mm comes back
  as 42.0000038), so `materialx_teapot` and `materialx_lion` move by about 1e-7 of their
  field of view: a 10.1 difference to measure. An unauthored `clippingRange` is USD's
  fallback `(1, 1000000)` as Hydra gives it, where the old reader clipped nothing; the
  three MaterialX close-ups, whose subject is nearer than one unit, author
  `(0.01, 1000000)`, and `render-settings.md` says so. A `focusDistance` of 0 (its
  fallback) still reads as Crust's 10 for a lens.
- **Meshes read Hydra's mesh schema and primvars (task 5.2.4).** Constant primvars are
  inherited, as the flattening has them, so a primvar var reads an ancestor's constant
  primvar instead of reporting it not inherited; this change gains an `aovs` delta saying
  so (a point instancer's per-instance primvar is still reported). A mesh's own
  `primvars:crust:subdivisionLevel` (inherited) replaces the stage's level for it in
  uniform subdivision, as the renderer-attributes requirement's "An inherited
  subdivision level" asks; the bare per-prim attribute and its `subdiv.legacy_level`
  warning go (`crust check` reports the attribute instead). `crust:rayMask` and
  `crust:motion:translate` move with the meshes, as spheres and implicit shapes share
  their readers. A primvar's type in a warning is spelled from its value and role
  (`point3f[]`).
- **Lights read their Hydra `light` data (task 5.2.7).** Parameters come with UsdLux's
  fallbacks, so a `DistantLight` that authors no `inputs:intensity` now emits 50000, as
  in Storm and hdPrman, where the old reader used 1; no sample is affected. A light
  parameter's colour space (its colour, its texture file) is read from the light's
  material network, where UsdImaging carries it.
- **Volumes read constant primvars and Hydra's cube size (task 5.2.8).** The
  `crust:volume:*` properties are `primvars:crust:volume:*`, and samples and tests are
  migrated. A `Cube` volume's box is Hydra's `cube/size`, whose fallback is USD's 2: an
  unsized volume `Cube` now spans `[-1, 1]` where the old reader used the unit cube. Every
  sample authors `size`, so no image moves. Any other prim keeps the unit cube.
- **Materials read their network only (task 5.2.9).** A shader is no prim of the index,
  so every value is a node parameter as UsdImaging builds it. Four behaviours follow
  from that:
  - `PxrDisneyBsdf` reads its own parameters (`specTrans`, `opacity`, `isThin`, …),
    which the island connects to the material's inputs. Before, it read the Material's
    inputs by their interface names. `refractionGain`, which no island shader connects,
    is no longer read. Its `baseColor` is read through the `PxrColorCorrect` it
    connects to.
  - A colour Ptex is the `PxrPtexture` or `HwPtexTexture_1` that the base colour
    connects to, directly or through a `PxrColorCorrect`. Before, it was the
    Material's `inputs:surfaceMap`, read whether or not a shader used it. All 114
    island materials that author `surfaceMap` reference `PtexBaseMaterial`, so the
    island binds the same files. A Ptex material's `baseColor` is then hidden by the
    Ptex connection, and only matters where the Ptex reads nothing. `ptex_quads.usda`
    and the Ptex fixtures gain a `PxrPtexture` node.
  - A connected input carries no constant of its own. So a declined `UsdUVTexture`
    without a `fallback` reads the `UsdPreviewSurface` default, and a displacement
    connected to an unusable texture displaces nothing (two tests renamed).
  - A parameter's colour space is Hydra's consolidated one, so a `colorSpace:name` on
    an ancestor now applies to `PxrDisneyBsdf`'s base colour too.

  `samples/pxr_displace.usda` changes for the first reason: its `PxrDisneyBsdf` authors
  `baseColor` and `roughness` on the shader, which the old reader ignored, rendering the
  defaults. They now apply (3 231 of 38 400 pixels, the quads, darker and rougher;
  deterministic, not noise). The other goldens hold.

  The material's `crust:displacementBound` is its constant primvar, which Hydra gives
  a Material prim. MaterialX node types come from the nodedef name alone, since the
  network carries no output type.

- **Pass collections read Hydra's expressions only (task 5.2.10).** Whether a
  collection restricts anything is now whether its `membershipExpression` differs from
  its fallback: nothing for `matte` and `prune`, `~//*.*` for `cameraVisibility` and
  `renderVisibility`. The authored `includes`, `excludes` and `includeRoot` are no
  longer read. The in-instance warning reads the paths the expression names. A target
  inside an instance that sits beside its own instance is subsumed by the instance's
  `/World/Rock//`, so Hydra names nothing to warn about. `render_passes.rs` now tests
  the inner target on its own, which matches no instance, as the delta spec says. The
  streamed-chunk warning `render_pass.nested_not_composed` is retired with streaming,
  and its row with it.

## Renderer attributes authored today (task 5.1)

The `crust:*` attributes and relationships that `samples/*.usda` and the `#usda`
fixtures under `crates/*/tests` author, by family and where they move (D5). Shader
identifiers (`crust:openpbr`, `crust:blur`, …) and EXR stamp keys (`crust:spp`, …) are
values, not attributes, and stay.

| family | authored names | files | from this change |
|---|---|---|---|
| render settings | `crust:samplesPerPixel`, `maxDepth`, `minSamplesPerPixel`, `varianceThreshold`, `frame`, `samplingStrategy`, `sampler`, `lightSelection`, `lightSamples`, `lightSamplesIndirect`, `pixelFilter`, `pixelFilterRadius`, `indirectClamp`, `pathGuiding`, `guidingTrainIterations`, `guidingProb`, `adaptiveNeighbourTolerance`, `display`, `subdivisionLevel`, `subdivisionEdgeLength`, `enableExposureCompensation`, `domeLightCameraVisibility` | most samples; `usd_scene`, `usd_inline`, `usd_adaptive`, `primvar_aovs`, `live`, … | unchanged, read from `namespacedSettings` |
| settings relationships | `rel crust:dicingCamera`, `crust:sampleFilters`, `crust:displayFilters` | `precomp.usda`; `precomp.rs`, `usd_adaptive.rs`, `live.rs` | `string` / `string[]` paths |
| products and vars | `crust:aov:*`, `crust:filter:*`; `rel crust:filter` | `aovs*.usda`, `precomp.usda`; `precomp.rs`, `usd_products.rs` | attributes from `namespacedSettings`; the relationship a `string` path |
| geometry | `crust:rayMask`, `crust:subdivisionLevel`, `crust:subdivisionEdgeLength`, `crust:displacementBound`, `crust:motion:translate` | `subdivision*.usda`, `displacement.usda`, `pxr_displace.usda`, `primvar_aovs.usda`, `materialx_teapot.usda`, `materialx_showcase.usda`, `precomp.usda`, `motionblur.usda`, `motionvector.usda`, `light_visibility.usda`; `usd_scene`, `usd_inline`, `usd_displacement`, `time_motion`, `motion_vector`, `primvar_aovs`, `light_linking`, `hidden_lights`, `render_passes` | constant `primvars:crust:<name>` |
| volumes | `crust:volume:*` (type, sigmaS, sigmaA, densityScale, anisotropy, emission, grid, noise) | `fog.usda`, `smoke.usda`, `aovs_lpe.usda`; `usd_inline`, `warnings`, `time_motion`, `light_linking`, `hidden_lights`, `render_passes`, `usd_listing` | constant `primvars:crust:volume:*` |
| lights | `crust:rayMask`, `crust:light:cameraVisible`, `crust:light:lpeTag` | `usdlux.usda`, `light_visibility.usda`, `veach_mis.usda`, `openpbr_showcase.usda`, `aovs_lpe.usda`, `precomp.usda`, `motionblur.usda`; `hidden_lights`, `light_linking`, `render_passes`, `usd_inline`, `usd_scene` | unchanged, read through `light/<name>` |
| precomp graphs | free `Shader` prims under the settings | `precomp.usda`; `precomp.rs` | a `Material` with `outputs:crust:*` |
