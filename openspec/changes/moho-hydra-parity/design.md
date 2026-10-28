## Context

See proposal.md for the motivation. The state this design builds on (crust-render
main at `2b4d88e8`, Moho main at `510174c`):

- **Crust's import today** (`crates/crust-core/src/scene/usd_import/`, 25 500
  lines) is a walk (`index/walk.rs`) over Moho's chain. Each prim is dispatched by
  Hydra type from `visit_prim` in `mod.rs`.
  - **Generic data.** Readers mostly read Moho's generic data through `PrimData`
    (`index/data.rs`): about 110 `value()` calls and about 150 `custom_*()` calls
    across 20 files, keyed by USD attribute names.
  - **Transforms.** The import composes world transforms itself
    (`compose_index`, `xform.rs`).
  - **Light linking.** The import judges collections itself (`light_links.rs`,
    with per-input opinions).
  - **Streaming.** It streams by chunk: `index::Streamed`, `traverse_stage`'s chunk
    loop, `stream_roots`, `MIN_STREAM_CHUNKS`, `OutsideMaterials`, side stages
    (`MAX_SIDE_STAGE_OPENS`) and `MatSource`'s fallback `input_chain`.
  - **Dropped Moho pieces.** It relies on M10 (`MaterialNodeResolvingSceneIndex`),
    on M12 (`ProtoRoot.outer`, `proto_roots`, `enclosing_instance`,
    `PrimData::with_outer`) and on Moho's old native layout (`PROPAGATED_ROOT`
    filters in `walk.rs`, `data.rs` and `instancing.rs::proto_children`; the
    `"/ForInstancer"` skip in `materials.rs`' agreement test).
- **What Moho now offers** (Moho's `docs/architecture.md`, the archived
  `hydra-parity-*` designs, PRs #16 and #18 to #21):
  - `chain::standard(stage_index)`;
  - `chain::renderer_filters(index, &[RendererFilter::light_linking(),
    RendererFilter::dependency_forwarding()])`;
  - `PrimTypeNoticeBatchingSceneIndex::new(input, Some(priority))` with `flush()`;
  - `PrimManagingObserver::new(index, Some(factory))`, where the factory implements
    `PrimFactory::create_prim(&Added, &PrimManagingObserver)` and each managed prim
    implements `ManagedPrim::dirty(&Dirtied, &PrimManagingObserver)`. Releasing a
    managed prim is dropping it.
  - Its index is at time code 0 when new.
  - It keeps `__usdAttributes` and `__usdRelationships` as its one addition,
    which a Hydra host does not have.
- **The live session** (`crates/crust-render/src/session/`) diffs each owned layer
  (`delta.rs` `layer_diff`) and classifies the diff (`classify`, an allow-list on
  authored fields). It applies the change in place through `crust_core::live`, which
  rebuilds through the import's readers over a masked stage
  (`usd_import/live.rs`), or it re-imports. The `live-geometry-edits`,
  `live-session-host` and `live-session-time` changes (all tasks done, not
  archived) shaped it.
- **hdCrust** (`hydra/hdCrust/`, change `hydra-render-delegate`, 25/29) already
  observes Hydra's terminal scene index. It writes what it sees as USD layers
  (`scene.usdc`, `edits.usda`), and the Rust session imports those layers through
  Moho. A keyless UsdImaging adapter (`crustAttributesAdapter.cpp`) smuggles
  `crust:*` attributes through Hydra.
- **The parity machinery** of S2: 3 603 baseline files (74 155 lines, 23 MB),
  `crust-core/src/parity.rs`, the `parity` feature (on in `crust-core`,
  `crust-render` and `crust-hydra` dev-dependencies), `World::parity_digest`,
  `Provenance::parity_digest`, `crust-render/tests/import_parity.rs`,
  `scripts/import_parity.sh`, `scripts/parity_images.sh` and its expected list. S2
  grew code by about 4 700 lines net, baselines aside.

## Goals / Non-Goals

**Goals:**
- One delegate whose code would run unchanged over any implementation of Moho's
  `SceneIndex` trait. Moho in Rust is the first such implementation; Hydra's terminal
  scene index through FFI is the next (D10).
- Update latency first: an edit costs what it touches, and the delegate keeps its
  per-prim state between updates.
- A smaller codebase: about −3 400 lines of code net, beside the baselines ("Size").

**Non-Goals:**
- Moving hdCrust onto the delegate (D10, the next change).
- Any change to the integrator, the kernel, materials evaluation or the image
  outputs.
- A chunked or streamed import of Crust's own (D6).
- Light filters, skinning, coordinate systems, draw modes: still unread, as before.
- Applying a time change in place beyond what the dirtied locators allow (D9).

## Decisions

### D1. The delegate: Moho's three steps, then a commit

```
stage ─ StageSceneIndex ─ chain::standard ─ renderer_filters(light linking @4,
        dependency forwarding @1000) ─ PrimTypeNoticeBatchingSceneIndex(priority)
        ─ PrimManagingObserver(CrustPrimFactory) ─► managed prims ─► commit ─► Scene
```

- **A new module `crates/crust-core/src/scene/hydra/`** holds the delegate:
  - `delegate.rs`: building the chain over a given `SceneIndexHandle`, `update()` =
    `flush()` then commit;
  - `factory.rs` and `prims/`: one managed prim kind per Hydra type family;
  - `commit.rs`;
  - the family readers, which move there from `usd_import/` (`mesh`, `shapes`,
    `implicit`, `strands`, `lights`, `camera`, `materials`, `preview`,
    `mtlx_network`, `settings`, `products`, `precomp`, `passes`, `volume`,
    `instancing`, `adaptive`, `assets`).
- **`usd_import/` becomes the Moho host.** It keeps:
  - opening the stage (`stage_builder`, the `mmap` resolver), building the
    `StageSceneIndex`, setting the time;
  - `stage_facts.rs`, the host facts: layers, the `.mtlx` reference, arc sources for
    provenance, openusd's errors behind `xform.uncomposable`, and the
    legacy-attribute check (D5);
  - `listing.rs` (`crust ls` walks the stage, not an import).
- **What the delegate may not name.** `scene/hydra/` names no `moho::stage`, no
  `moho::openusd::usd`, and no `moho::tokens::usd` (the generic data). A test
  `crates/crust-core/tests/hydra_only.rs`, shaped like `one_openusd.rs`, fails
  otherwise. Host facts reach the delegate as a plain struct (`HostFacts`) the host
  fills, and a host without a stage (hdCrust, D10) leaves it empty.
- **Priorities** (`PrimTypePriority`, the order a flush syncs types in):
  `material` → light types → `lightFilter` → geometry types → `instancer` →
  `camera` → `renderSettings`, `renderProduct`, `renderVar`, `renderPass`. This
  matches hdPrman: what is bound or linked first. The commit does not depend on it.
- **Alternatives rejected:**
  - *Keep the walk and read Hydra schemas in it.* A walk is not what a Hydra host
    gives, and updates would still be a second implementation.
  - *Make each managed prim build its kernel geometry at once.* The bake-or-instance
    decision and the geometry ids need every prim (D3).

### D2. Managed prims record; the commit reads

`dirty()` records the locator set and marks the prim stale. It reads nothing,
because a batch can dirty one prim several times. At the commit, each stale prim
syncs: it reads its data through the observer's `scene_index()` on the index's
thread, and rebuilds its translated state. The prim kinds are:
- `Mesh`: the interned mesh, the material binding, the placement;
- `Curves`;
- `Sphere`;
- `Implicit`;
- `Volume`: a prim with `primvars:crust:volume:type`;
- `Light`;
- `Material`;
- `Camera`;
- `Instancer`;
- `RenderSettings`, products, vars and passes.

Removal drops the prim, and the commit sees it gone. A prim carrying `instancedBy` is
a prototype part. It is never placed at its own path, only through its instancer
(D7).

### D3. The commit: deterministic, path-ordered, batch or live

- **Ids in path order.** Geometry ids, light-list order and volume order follow
  `sdf::Path` order over the managed prims. A commit is therefore a function of the
  managed prims' state. This is what makes "an update in place ↔ a full re-import"
  hold by construction: both commit the same states.
- **Why not stage order.** Moho's merging promises no child order, and the
  instancing propagations merge. Path order changes the light-list order, so images
  change at noise level once. The goldens are re-recorded after a 1/√N check (D11).
- **The first-camera fallback** (no `--camera`, no settings camera) keeps today's
  depth-first order, by one walk of `children()` made only in that case. That way no
  stage silently changes camera.
- **Bake or instance.** The commit makes the `flush_meshes` decision (placed once:
  baked; otherwise instanced) over every managed mesh and instancer, and adaptive
  placements are counted over the whole stage. The old per-top-level-subtree count
  was streaming's: its scenario now reads "placed once on the stage". ALab's adaptive
  images may change, and the change is listed.
- **Batch mode** (`crust render`, `check`, `diagnostic`): the commit consumes the
  managed state. No geometry is kept twice, and the index and stage are released
  before rendering, as `release_stage` does now.
- **Live mode** (the session): the state is kept, and `keep_geometry` is implied, as
  `live-geometry-edits` D2 has it.

### D4. Reading Hydra, not USD

Each family reads Hydra's locators and nothing else:
- mesh: `mesh/topology`, `mesh/subdivisionScheme`, `mesh/subdivisionTags`,
  `mesh/doubleSided`, and `primvars/<name>/{primvarValue, indexedPrimvarValue,
  indices, interpolation, role, colorSpace, elementSize}` (`points`, `normals`,
  `widths`);
- curves: `basisCurves/topology`, and `nurbsCurves`;
- implicit shapes: `sphere`, `cube`, `cylinder`, `cone`, `capsule`, `plane`;
- every prim: `xform/{matrix, resetXformStack}` (flattened, sampled over the shutter
  through `Sampled::sample_times`), `visibility/visibility`, `purpose/purpose`,
  `materialBindings/<purpose>/path`, `categories`, `instancedBy`, and
  `primOrigin/scenePath` for naming;
- material: `material/<context>/{nodes, terminals, interface, config}`, with
  `nodeIdentifier`, `parameters/<name>/{value, colorSpace}` and `inputConnections`;
- light: `light/<name>` (Hydra's tokens and any attribute name), `light/lightLink`,
  `light/shadowLink`;
- camera: `camera/*`;
- instancer: `instancerTopology/{prototypes, instanceIndices, instanceLocations,
  mask}`, the instance primvars (`hydra:instanceTransforms`,
  `hydra:instanceTranslations`, `…Rotations`, `…Scales`) and `instanceCategories`;
- render settings: `renderSettings/{renderProducts, namespacedSettings,
  includedPurposes, materialBindingPurposes, renderingColorSpace, shutterInterval,
  active}`;
- render pass: `renderPass/{passType, renderSource}` and the pass prim's
  `collections`;
- the root: `sceneGlobals/{startTimeCode, endTimeCode, timeCodesPerSecond,
  activeRenderSettingsPrim}` and `__usdUpAxis`.

The authored-versus-fallback rule of `PrimData::value` goes: Hydra's value is taken
as it is, fallback included. `metersPerUnit`, which Hydra does not carry, is a host
fact, as Moho's architecture says. Motion warnings that needed `mightBeTimeVarying`
now read whether `sample_times` over the shutter gives more than one time.

### D5. Renderer attributes: where each `crust:*` goes

| today (read from `__usdAttributes`) | from this change | Hydra carries it as |
|---|---|---|
| `crust:rayMask`, `crust:subdivisionLevel`, `crust:subdivisionEdgeLength` (on a prim), `crust:displacementBound`, `crust:motion:translate`, `crust:volume:*` | constant primvar `primvars:crust:<name>` | `primvars`, flattened to descendants |
| settings `crust:*` attributes (`samplesPerPixel`, `maxDepth`, `sampler`, …, `display`) | unchanged | `renderSettings/namespacedSettings` |
| product and var `crust:*` attributes (`crust:aov:raw`, `crust:filter:*`, …) | unchanged | the product's and var's `namespacedSettings` in `renderProducts` |
| `rel crust:dicingCamera`, `rel crust:sampleFilters`, `rel crust:displayFilters`, `rel crust:filter` | `string` / `string[]` attributes holding absolute paths | `namespacedSettings` |
| `crust:light:cameraVisible`, `crust:light:lpeTag` | unchanged | `light/<name>` |
| precomp graphs: free `Shader` prims | a `Material` with `outputs:crust:sampleFilter` or `outputs:crust:displayFilter` | `material/crust/...` |

- **Why primvars.** They are how Hydra delegates take renderer object settings
  (hdPrman's `primvars:ri:attributes:*`, arnold-usd's `primvars:arnold:*`, Karma's
  `primvars:karma:object:*`). They reach every Hydra host without a plugin, and they
  inherit as `crust:*` attributes did. The keyless adapter hdCrust carries today
  serves UsdImaging hosts only and must be installed. Solaris and husk do not always
  run UsdImaging on the original prims.
- **Why string paths.** UsdImaging's `namespacedSettings` lists namespaced
  attributes, and relationships only for four `ri:` names (Moho's stage design, D8
  deviations). A Hydra delegate cannot see `rel crust:*`. Strings lose path
  remapping across references; render settings rarely live in a referenced asset.
  An alternative (asking Moho for relationships) would break Hydra parity, which is
  Moho's principle.
- **Why `Material` for precomp.** A free `Shader` has no Hydra data: UsdImaging
  represents shaders by their material. hdPrman's sample filters come through a
  RenderMan-specific adapter plugin that Moho does not port. A `Material` whose
  outputs are in the `crust` namespace gives a network in context `crust`, the way
  `mtlx` gives one.
- **Legacy authoring is reported, not read.**
  - `crust check` runs a host-side stage walk (`stage_facts::legacy_crust`) and
    raises `attribute.legacy_crust` per prim, naming the replacement.
  - `crust render` does not walk, so first-load time is untouched.
  - Samples and fixtures are migrated in the change.
  - The site's `usd/` pages carry a migration table.

### D6. One stage, no chunking of Crust's own

Streaming existed because composing Moana cost openusd 75.74 GiB (the
`usd-scene-import` design record). openusd has since been fixed: Moho's update
benchmark composes Moana `LoadAll` at a 7.01 GiB peak, its chain included. Crust's
own structures dominate Moana's peak (24.18 GiB at S2's end, against a 1.01× ratio
for the direct importer, which streamed too). So the memory bound streaming bought
is now mostly openusd's to give, and a single stage is what a Hydra delegate is
handed.

Rejected alternatives:
- *Load and unload payloads in batches through one stage.* Unloading removes prims
  from the index, and a Hydra delegate releases what it built for removed prims.
  Keeping baked geometry for removed prims would break the delegate model and live
  updates.
- *Crust-managed masked stages.* This would recreate what Moho deleted.

If Moana's re-measured peak (task 9.3) proves too high, the next lever is the managed
prims' kept state in batch mode (D3 already consumes it), not chunking.

### D7. Instancing on Hydra's layout

- **Native instancers.** A native instancer is the prim
  `…/UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer`. Its prototype is
  `…/UsdNiInstancer/UsdNiPrototype`, whose prims carry `instancedBy`. A point
  instancer's prototypes are its `ForInstancer<hash>` copies, named by
  `instancerTopology/prototypes`.
- **The `Instancer` managed prim** places each prototype's parts by
  `hydra:instanceTransforms` (native) or the translate, rotate and scale primvars
  (point), masked by `mask`. It composes its own world `xform` (identity when
  absent, D4 and the spec's "Missing data reads as its default").
- **Nesting.** A nested instancer is a prototype part of its outer instancer
  (`instancedBy`). `MAX_INSTANCE_NESTING` counts `instancedBy` depth.
- **Prototype keys.** Parts are cached by their prototype prim path. That path is
  stable for a stage, and there is one stage. This retires the Invariants pair
  "keyed by Moho's propagated prototype path, stable across a streamed import's
  stages" (`ProtoRoot::key`, `ImportCaches.placements` per subtree). The pair
  becomes "keyed by the prototype's Hydra path; a name for warnings and provenance
  comes from `primOrigin/scenePath`".
- **Moana's coral.** With M12 gone, a point instancer's target inside a native
  instance copies as an empty prim (UsdImaging 2.0), and `isCoral`'s coral renders
  nothing. It is accepted and listed. The fixtures that pinned M12
  (`instanced_instancers.rs`:
  `a_point_instancer_targeting_prims_inside_an_instance_places_them`,
  `…target_inside_an_instance_keeps_its_material`,
  `an_instanced_point_instancer_target_inside_an_instance_keeps_its_material`)
  become the inverse: the target places nothing.
- **Flattened data on an instancer.** A native instancer may or may not carry
  flattened data. Hydra gives isCoral's instancer an identity `xform` and visible,
  and Moho gives none (Moho's parity check, "Paused" on Moho main; exempted on its
  `fix/hydra-parity-check` branch, PR #21). Missing means default makes both read
  the same.

### D8. Light linking from categories

The light linking filter gives ids (`group_<n>`). The `Light` prim reads
`light/lightLink` and `light/shadowLink`. A geometry's receiver mask comes from its
`categories`, or, for an instanced part, from its instancer's `instanceCategories`
at the instance (native) or `categories` (point). The commit turns the ids into
Crust's link tables (`encode_shadows`, `demote` and `index_remap` stay).
`LightLinks::light_added`, `opinions`, `link_query`, `inside_instance`, and the
agreement tests against Moho go, about 600 lines. Expressions with USD-only
predicates now match nothing, as in Hydra (Moho audit §4). The `light_link.*`
warnings that judged collections (unreadable collection, target inside a prototype,
cycle) are kept only when the delegate can still observe the cause, and the rest
are retired from `warnings.rs` and the reference page.

### D9. The live session on the delegate

- **One stage, one delegate.** The session keeps one stage and one delegate. `apply`
  diffs each owned layer that changed (`layer_diff`, kept). It turns the difference
  into a `usd::Diff`:
  - `Edit::Create` for a new spec with all its fields;
  - `SetField` for a changed field;
  - `EraseField` for a field that went;
  - `RemoveSpec` for a spec that went;
  - parents created before children, and children removed before parents.
  It applies the diff with `Stage::apply_diff(&diff, ApplyMode::ExactLayer(id))`,
  then runs `apply_pending_updates` and `update()`. The diff is replayed onto a layer
  the stage already composes, so composition does the rest. An edit to a referenced
  prim reaches every composed copy. The `arc_sources` bookkeeping of
  `live-geometry-edits` D6 becomes unnecessary and goes.
- **Classes from what the index reported.** The batch's added, removed and dirtied
  entries, per managed prim kind and locator, map to the classes:
  - `settings`: `renderSettings/*`, a render product or var, or the render camera's
    `camera/*` or `xform`, unless the dicing projection moves;
  - `materials`: `material/*` on a material in use, when the opacity, transmission
    and emission class stays;
  - `lights`: a light's `light/*` or `material/*`; an added or removed light; a
    distant or dome light's `xform`; `categories` or `instanceCategories` alone;
  - `geometry`: a mesh's `xform` or `primvars/points|normals`, `mesh/*` within
    `live-geometry-edits` D4's conditions, and an area light's `xform` or shape;
  - `full`: anything else, including a resync of a non-light prim, `materialBindings`
    and `visibility`.
  The decision table lives in one function (`hydra::commit::classify`), beside the
  commit it predicts. `classify` and its allow-list on authored fields go.
- **`full`.** It stays a re-import into a fresh stage and delegate, the proven
  fallback. An external source layer changing on disk (`live-session-host` D2) is
  still `full`.
- **The pair.** In place, the commit runs over the updated managed prims; in `full`,
  over fresh ones. The bit-identity tests of `crust-core/tests/live.rs` and
  `session/live_tests.rs` stay and grow: a light added, a linked light added.
  `Signature` in `rt_world.rs` stays the check that a material edit did not move what
  the world baked.
- **Time.** `set_time` (`live-session-time`) sets the index's time, which dirties
  only what might vary. The same table decides the class. A time change that dirties
  only cameras, lights or materials is applied in place. This is
  `live-session-time`'s follow-up, reached through the same path rather than built
  on purpose. `Imported::update` no longer refuses another time when the table
  accepts it.
- **What goes.** `usd_import/live.rs` (869 lines), and in `crust_core::live` the
  `Change`, `Update` and masked-stage rebuild. The `Provenance` that `pick`, `probe`
  and hit names read stays.

### D10. hdCrust: the next change, `hdcrust-scene-index`

This change makes the delegate's Rust code host-independent. It does not yet run the
delegate over Hydra's terminal scene index in hdCrust. Reasons:
- **Unsafe and threading.** The next step is an FFI implementation of Moho's
  `SceneIndex` trait over an `HdSceneIndexBase` (prims, children, container and
  sampled data sources, and observer callbacks). That is new audited `unsafe` in
  `crust-hydra/src/ffi.rs`, a C ABI for data sources in `include/crust_hydra.h`, and
  a thread model: Hydra's data sources are thread-safe, Moho's are `Rc` and
  thread-confined, and the session thread must own every read. It is its own design
  problem, best done once the delegate it feeds has settled.
- **It is checkable apart.** The delegate's correctness (Hydra-only reads, in-place
  equals fresh) is pinned in Rust without OpenUSD C++. The next change's gate is
  hdCrust's `compare.sh` against `crust render`, which already exists.
- **hdCrust keeps working in between.** The layers it writes are imported by this
  delegate through Moho. `primvars:crust:*` travel through Hydra as primvars and are
  written back by `sceneWriter.cpp`, which already writes primvars. Lights' `crust:*`
  are still carried by the keyless adapter and written back as attributes, which the
  `light` container reads by name. The one required edit: `sceneWriter.cpp` writes
  `crust:dicingCamera` as `string` instead of a relationship (task 6.5).
  `compare.sh`, `interaction.sh` and `usdc_roundtrip.sh` must still pass.
- **What the next change will do:**
  - hdCrust's `SetTerminalSceneIndex` hands the terminal index to `crust-hydra`;
  - `Update` drives the delegate's update and commit;
  - `scene.usdc` and `edits.usda` and their writer retire, along with the keyless
    `crust:*` adapter (lights' `crust:light:*` stay readable through
    `LookupLightParamAttribute`);
  - the `hydra-render-delegate` spec's "renders a layer crust can render" becomes
    "renders what `crust render` renders of the same stage".
  That change also settles `hydra-render-delegate`'s open tasks 1.2, 3.6, 6.1 and
  7.1, or archives it first.

### D11. Removing the parity machinery; the goldens are the guard

The order is chosen so that each step has a guard:
1. On the current Moho, record goldens: `scripts/check_images.sh record`.
2. Remove the parity machinery. Its removal changes no image.
3. Remove streaming. With streaming off, the import already equals the streamed one
   (its spec), so the goldens hold.
4. Bump Moho, then move readers family by family. Each image difference must be
   explained by an exception the spec lists:
   - Moana's coral, purpose, `over` prims, the default time, adaptive counts;
   - path-ordered ids: a noise difference that falls as 1/√N across 16, 64 and 256
     spp, per CLAUDE.md.
   Then re-record the goldens and write each explained difference in the
   `usd-scene-import` design record.
5. Once the delegate replaces the walk, the goldens must hold bitwise.

`crust-core/tests/layering.rs` loses `parity` from the `import` layer and gains
`hydra`.

### D12. The update-latency benchmark

- **Where.** `crates/crust-render/examples/update_latency.rs` (feature `session`),
  with a cookbook recipe in `openspec/specs/cli/design.md`.
- **The fixture.** It is Moho's: a camera with a translate op and N meshes under
  `/world`; the linked light includes `/world/a`.
- **Moana.** `island.usda`, camera `/island/cam/shotCam`. The linked light includes
  Moho's prim (`/island/isCoral/isCoral5/geometry/hires_part/starhornstem0004_geo`),
  so the figures line up with Moho's.
- **One run.**
  1. Write the owned layer with the edit.
  2. Call `LiveSession::apply()`.
  3. Stop the clock when `apply` returns and `render()` has moved. `apply` is
     synchronous on the session thread and restarts the render before it returns.
  An added light is removed after its run, untimed.
- **The split.** Each run also reports, from the session's debug phases: layer diff
  and replay, Moho (`apply_pending_updates` and `flush`), Crust's commit, and the
  restart (cancel, join, spawn). It also reports the class. The output is the median
  (the mean of the two middle runs for even N) and the worst over N runs, as Moho's
  is.
- **Reported, not gated.** Results go to the `mcp-session` design record, §
  "Update latency", beside Moho's figures for the same edits.

### D13. First load, re-measured

ALab and Moana are run with `crust render --stats -s 16` (peak memory, `Parse USD
stage` and the delegate's phases: population, flush, commit) at the change's end. The
figures are recorded against S2's end (ALab 21.14 GiB and 57.1 s; Moana 24.18 GiB
and 2 171 s) and the direct importer's. `MatSource`'s rebuilt chains on misses (39
on `isHibiscus`) disappear with M10 and side stages: a material's networks are read
once from its `material` data. Reported, not gated. Update latency ranks above it
(proposal).

## Size

Estimated from the files this change deletes or rewrites. Measured at the end with
`git diff --shortstat main` (task 10.4).

| | lines |
|---|---|
| parity machinery (`parity.rs`, `import_parity.rs`, two scripts and the expected list, digests, feature wiring) | −1 250 |
| streaming (`Streamed`, the chunk loop, `OutsideMaterials`, side stages, `subtree_roots`, streamed tests) | −1 100 |
| the walk, `PrimData`'s generic accessors, M12 and the old layout filters | −900 |
| `light_links.rs` judging collections → categories | −600 |
| `xform.rs` composition → flattened `xform` (motion over the shutter stays) | −500 |
| masked-stage rebuilds (`usd_import/live.rs`, `crust_core::live`'s `Change`/`Update`, arc sources) | −1 000 |
| `materials.rs`, `instancing.rs` and `passes.rs` on Hydra's data | −900 |
| `classify`'s allow-list → the locator table | −100 |
| removed | **≈ −6 350** |
| delegate (factory, managed prims, commit, priorities, `HostFacts`) | +1 800 |
| layer diff → `usd::Diff` | +250 |
| legacy `crust:*` check | +120 |
| update-latency example | +400 |
| new tests (delegate pairs, Hydra-only, categories, time 0, instancing layout) | +900 |
| added | **≈ +3 470** |
| **net, code** | **≈ −2 900 (±1 500)** |
| baselines (`tests/data/import_baselines/`, 3 603 files, 23 MB) | **−74 155** |

S2 added about +4 700 lines net, baselines aside. This change takes back about
two-thirds of that, and all of its 74 155 lines of baselines.

## Risks / Trade-offs

- **[Breaking scenes]** Authored `crust:*` attributes on geometry, `rel crust:*`
  settings and precomp shaders stop being read.
  → `crust check` names each one and its replacement; samples, fixtures and the
  site's pages migrate in the change.
- **[Images move once]** Path-ordered ids, Hydra's purpose, `over` prims, time 0 and
  whole-stage adaptive counts change some images.
  → Each difference is explained against the listed exceptions, the noise ones by
  1/√N, before the goldens are re-recorded (D11).
- **[Moana memory without streaming]** The peak could rise.
  → It is measured (D13). Batch mode consumes managed state. If the peak is too high,
  that is reported, not hidden.
- **[A Hydra quirk Crust must not fix silently]** A Crust fix to Hydra's behaviour
  would be a renderer filter of its own (Moho's principle), never a read of generic
  data.
  → `hydra_only.rs` forbids the latter.
- **[The locator table under- or over-classifies]** A wrong class gives a stale
  render or a slow one.
  → Every in-place class keeps its bitwise pair test against a fresh import. A change
  the table does not know is `full`.
- **[The diff replay misses a field kind]** Fields that `apply_diff` cannot replay
  fail it.
  → On a replay error, the session falls back to `full` and logs once at debug.
- **[The change is large]**
  → Its phases (tasks 1–10) each end green, and the change can stop after the bump
  phase (task 3) with a working importer if the delegate phase needs revisiting.

## Migration Plan

- **Prerequisite.** Archive `live-geometry-edits`, `live-session-host` and
  `live-session-time` (all complete) before this change, so that the `mcp-session`
  delta modifies their text.
- **Order.** Land in the task order. The Moho bump is its own commit (CLAUDE.md:
  Moho, then crust-render, then Mantle). Mantle bumps crust-render after this change.
- **Rollback.** Revert the merge. Moho's pin returns with it.
- **The changes in flight:**
  - `add-precomp-filters` (21/22): its authoring changes here (D5), and its spec text
    should be updated when it archives.
  - `payload-and-purpose-view` (0/32): its tasks that name `CRUST_STREAM_IMPORT` are
    stale.
  - `live-pick-and-camera-retarget` (9/10): its picks still read `Provenance`, which
    stays.

## Open Questions

- Whether Moho should expose a way to read `metersPerUnit` that a Hydra host also
  has. hdCrust's next change needs it from Hydra (usdview passes it in the render
  pass state), and it does not change this change's host fact.
