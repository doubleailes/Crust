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
  (Moho now, Hydra's terminal index through FFI next, D10).
- Update latency first: an edit costs what it touches.
- A much smaller codebase ("Size").

**Non-Goals:**
- Running the delegate over Hydra's index in hdCrust (`hdcrust-scene-index`, D10).
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
| `crust:light:cameraVisible`, `crust:light:lpeTag` | unchanged | `light/<name>` |
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
    `xform`, unless the dicing projection moves. A camera retarget is a
    `renderProducts` change, so `settings` (`live-pick-and-camera-retarget`'s
    retarget, reached here).
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

### D10. hdCrust and `crust-hydra` until `hdcrust-scene-index`

`crust-hydra`'s `Session` wraps `LiveSession` today, which this change removes. Two
options were weighed: letting hdCrust break between the two changes, or keeping the
minimal path. The minimal path is chosen, because it keeps `.github/workflows/hydra.yml`
green and usdview usable at the cost of about 150 lines.

- **The minimal path.** hdCrust's C++ still writes `scene.usdc` and `edits.usda` and
  calls `crust_hydra_apply`. `crust-hydra` re-imports `edits.usda` through the batch
  host, so every change is `full`, and renders on the render-control API.
- **Tests.** `compare.sh` and `usdc_roundtrip.sh` still pass. `interaction.sh`'s
  expected classes become `full`, and its bitwise comparisons stay.
- **The one C++ edit.** `sceneWriter.cpp` writes `crust:dicingCamera` as a string.
- **Why not more.** The next change, `hdcrust-scene-index`, replaces all of this:
  - an FFI `SceneIndex` over Hydra's terminal index, with new audited `unsafe` and a
    data-source ABI;
  - `SetTerminalSceneIndex` and `Update` driving the delegate;
  - the layer writer and the keyless `crust:*` adapter retired.
  Doing it here would put the C++ thread and ABI design inside an already large
  change.

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
| `crust-hydra` session wrapper and its tests, slimmed | −200 |
| removed | **≈ −19 260** |
| delegate scene side (factory, managed prims, commit, class table, `HostFacts`) | +1 900 |
| render control (`crust-render/src/delegate/`) and its tests | +1 100 |
| `crust check` stage checks (legacy `crust:*`, path across an arc, default and samples) | +250 |
| `crust-hydra` minimal path | +150 |
| update-latency example | +400 |
| new tests (Hydra-only, categories, time 0, instancing layout, in-place pairs) | +900 |
| added | **≈ +4 700** |
| **net, code** | **≈ −14 500 (±2 500)** |
| baselines (3 603 files, 23 MB) | **−74 155** |

S2 added about +4 700 lines of code net. This change removes about three times that,
and all of S2's baselines. The lock file also loses `rmcp`, `tokio` and their trees.

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
- **[hdCrust is slow between the changes]** Every edit is a full import.
  → Accepted until `hdcrust-scene-index`.
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
- **`hydra-render-delegate` (25/29) and `hdcrust-scene-index`.** The next change,
  `hdcrust-scene-index`, runs the delegate over Hydra's terminal index. It retires the
  parts of `hydra-render-delegate` it makes obsolete:
  - D2: the two layers on disk;
  - D3: prim translation and the keyless `crust:*` adapter;
  - D5: "one change at a time" through the session's classes;
  - D6: the session core in `crust-render`'s library;
  - the spec requirements "The delegate renders a layer crust can render", "The
    flattened layer carries the scene Hydra presents" and "Edits follow the session's
    classes".
  Its open tasks 1.2, 3.6, 6.1 and 7.1 are settled or dropped there. This change
  leaves `hydra-render-delegate`'s artifacts untouched.
- **Mantle.** Its `mantle-crust` integration over `LiveSession` breaks with this
  change and is rebuilt on Mantle's side over a Hydra viewport scene index. The MCP
  server becomes a Mantle feature. Neither is planned here.

## Open Questions

- Whether Moho should expose a `metersPerUnit` that a Hydra host also has. hdCrust's
  next change needs it from Hydra. It does not change this change's host fact.
