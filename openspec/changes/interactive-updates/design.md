## Context

See proposal.md for the motivation and the measured table. The state this design
builds on is crust-render main `c873beeb` and Moho `c808025`. `moho-hydra-parity` is
merged (PR #421) and its archive (PR #422) is open, so this change's `render-delegate`
deltas modify requirements that land with that archive.

What the code does today, as far as this change touches it:

- **The commit** (`scene/hydra/commit.rs::visit_all`, `commit_prim`) walks the managed
  prims in `sdf::Path` order. It emits a light inline (`emit_light` in
  `scene/hydra/mod.rs`): its emitter geometry is attached to the one `WorldBuilder` at
  the next geometry id, between the meshes around it. Ids, light-list indices and the
  light-link runs all follow path order (`moho-hydra-parity` D3).
- **Light links** (`scene/hydra/light_links.rs`):
  - `LightLinks` records every emitting prim's categories and geometry runs during the
    walk.
  - `resolve(self, …)` consumes that record before `WorldBuilder::commit`. It demotes
    lights linked to nothing (`LightList::remove`, `demote`) and writes a per-geometry
    light class (`set_light_classes`).
  - It also encodes shadow classes into bits 3–31 of every shadow caster's ray mask
    (`encode_shadows`, bits ranked by population).
  - The masks end up per primitive in the BVH leaves (`crust-rt/src/bvh/collapse.rs`
    `push_leaf`). The build itself reads no mask (`bvh/build.rs`).
- **The kernel** (`crust-rt/src/scene/mod.rs`) commits every top-level geometry into one
  BVH. `World::replace_geometry` (`rt_world.rs`) rebuilds that whole tree with
  `commit_ref` from the kept inputs (`WorldBuilder::keep_inputs`). Even an in-place
  geometry edit is therefore O(top-level primitives) in the tree build.
- **An update** (`scene/hydra/delegate.rs::SceneDelegate::apply`) classifies the batch
  (`commit::classify`).
  - In place, it rebuilds through `hydra/update.rs::rebuild` and `Update::apply`.
  - Otherwise it calls `self.commit` while still holding the old `scene` it was handed.
  - `classify` sets `change.recommit` for a light added or removed and for
    `categories` / `instanceCategories`.
  - A mesh reshape is in place only when `Provenance::reshapable` holds: baked, one
    placement, not `shared`.
- **Render control** (`crust-render/src/delegate/mod.rs::Delegate::update`):
  - It calls `assets.revalidate()` and `prepare`, stops the render, and reassembles the
    scene (`Held::into_scene`).
  - It sweeps the kept assets only when there is no scene.
  - It updates, then rebuilds `Held::of`: the `material_of_slot` map over every
    provenance slot, then `Renderer::new`, which builds the light selection.
  - `launch` then calls `Renderer::reconfigure`, which builds the light selection a
    second time.

## Goals / Non-Goals

**Goals:**
- A light added, removed or reshaped, and a receiver's categories, applied in place,
  bit-identical to a fresh delegate. The cost is O(lights + emitters), plus O(receivers)
  only when a link is restricted. No receiver geometry is read or rebuilt.
- A camera or settings edit whose Crust share does not grow with the scene.
- A points, normals or topology edit on a shared direct mesh or on a prototype's mesh,
  applied in place.
- A re-commit that holds one scene at a time and leaves resident memory where the
  first commit left it, within what the allocator retains (measured).
- The latency table re-run with targets and memory.

**Non-Goals:**
- First-load time, and ALab population (a Moho follow-up).
- A Hydra C++ host.
- Changes to Moho. Gaps found are listed (§ Moho gaps) for Moho to plan.
- Making a geometry edit's top-level tree rebuild O(changed) (refit, or a tree of
  per-prim segments). A geometry edit stays O(top-level primitives) in the tree build.
  The segment mechanism of D1 is where that would go later (Q1).
- A material added or removed, a mesh added or removed: these stay `full`.

## Decisions

### D1. The light sources are a layer of their own: ids after all receivers, a kernel segment of their own

The commit no longer emits a light where the walk meets it. `commit_prim` records the
light (path and world transform) in a new `ctx.light_prims`. After the walk, and before
`flush_meshes`, `emit_light_layer` runs `emit_light` over those prims in path order.
- The emitters take ids `[G, G + E)`, where `G` is the number of geometry ids the walk
  handed out.
- The light list holds every light in path order, as today.
- Light-list order and emitter id order are the same order.

In the kernel, `SceneBuilder` gains `close_segment()`, called between the walk's
geometry and the light layer.
- `commit` builds one BVH per segment.
- `Scene::intersect` traverses segment 0, then segment 1 with the running closest
  distance as its `t_max`. `occluded` stops at the first segment that hits.
- An equal distance keeps the earlier segment's hit, because the second traversal only
  accepts a strictly closer one.
- `Scene::rebuild_segment(1, geometries)` rebuilds the emitters' BVH alone. This is
  O(E log E).
- A scene with one segment is today's scene, layout and all. The CLI's
  procedural fallback and every test that builds a world by hand are untouched.

In `rt_world.rs`, `World` keeps `G` (`light_base`). `World::replace_light_layer(layer)`:
- truncates every per-id table to `G` (`materials`, `faces`, `light_classes`,
  `transparent_emitters`, `holdouts`, `primvars`, the kept inputs);
- appends the layer's;
- rebuilds segment 1;
- recomputes the world flags. `bounce_pass_throughs` and `bounce_side` depend on
  transparent emitters, so the receivers' part of each flag is kept from the commit and
  ORed with the layer's.

The emitters are not motion-blurred, so `World::motion` has no entry in `[G, G + E)`.
`check` refuses a layer that would add one.

*Why.* Path-ordered ids are what make an in-place update equal to a fresh commit by
construction. They are also why a light insert shifts every emitter and receiver after
it. Moving the emitters to the end of the id range confines the shift to the emitters.
A separate segment confines the tree rebuild to them as well. The fresh commit and the
in-place update both run `emit_light_layer`, so the bit-identity pair still rests on
one implementation.

*Alternatives.*
- **Keep emitters inline, re-commit the top-level tree on a light change.** This
  renumbers every id after the light (every per-id table, the provenance) and rebuilds
  the whole top-level tree: about the 19 s BVH commit on Moana, plus the renumbering.
  That is 13 minutes down to tens of seconds, not milliseconds. It is kept as the
  fallback if D1's second traversal costs too much throughput (Q3).
- **Append-only ids** (a new light takes the next free id and the last list index; a
  removal leaves a hole). This breaks the bit-identity pair, because a fresh commit
  orders by path. It is offered to the user as a relaxation (Q1, option R), not taken.
- **Emitters as kernel instances in the one tree.** The top-level tree would still be
  rebuilt for each added instance, so this was rejected.

*Image impact.* Emitter ids move, and hits on an emitter and a receiver at exactly the
same distance now resolve to the receiver (segment 0). Neither changes the estimator.
- The goldens are checked: with no area light, a scene is bit-identical.
- Any that move are shown to be noise, falling as 1/√N, as `moho-hydra-parity` D11 did.
- Every output that exposes a geometry id is listed and checked: the pick, light-path
  inspection by light index, and identity AOVs if any.

*Throughput.* A ray in a scene with area lights pays a second, small traversal setup.
It is measured three ways:
- `scripts/bench_ab.sh` on the samples with area lights (`cornellbox`, `usdlux`,
  `many_lights`);
- callgrind instruction counts on `cornellbox`;
- a run with no area light, which must be identical (one segment).

### D2. Light links: judged from a kept record, installed on a committed world

`LightLinks` is split in two:
- `judge(&record, &lights) -> LinkTables` is pure. It computes:
  - which lights leave the list (linked to nothing);
  - the per-receiver light class;
  - the shadow class bits;
  - the runtime `LightLinks` for the list;
  - the volume classes and masks;
  - the warnings.
- `install(LinkTables, …)` applies the tables to a `WorldBuilder` at the commit, or to a
  committed `World` in place.

The commit calls both, so the in-place path and a fresh commit share one judge and one
install.

The record (interned receiver paths, their categories, their geometry runs, the volume
runs) moves into `Provenance` (`crust-core/src/live.rs`) instead of being consumed. It
covers receivers only. The emitters' runs are the light layer's, and are rebuilt with
it. Its memory is O(emitting prims), the size of the `geometry` runs the provenance
already keeps.

Installing on a committed world:
- **Light classes** (`World::light_classes`) are a side table and are rewritten in
  place.
- **Shadow masks** live in the BVH leaves. They are rewritten by a new
  `crust_rt::Scene::set_mask(geom_id, mask)`. It visits the primitives of that geometry
  through the scene's per-geometry tables and patches each lane's mask, in the leaves
  and in the kept inputs.
  - This is valid because the build reads no mask. The intersection-kernel requirement
    "Ray masks rewritten in a committed scene" pins it, with a test that commits the
    same geometries under two mask sets and compares node layouts.
  - Patching is linear in the patched primitives, with no rebuild.
  - Only geometries whose encoded mask changes are patched. When no light was or is
    shadow-restricted, nothing is.
- **Demotions** (a light linked to nothing) act on the light layer only: an emitter's
  mask and transparent flag, or a backdrop.

Judging is O(receivers × restricted lights / 64) for classification, plus the patch.
- A light with trivial collections added to a scene with no restricted light needs no
  judging: the tables are extended with the light's unrestricted entry.
- One with a restricted link re-judges.
- A shadow-restricted one can move the bit ranking, which is by population and counts
  the emitters, so every caster whose bit moves is patched.

`categories` and `instanceCategories` dirtied on a receiver update that receiver's
interned categories (`receiver_categories`, the commit's reader) and re-judge. They
stop being `recommit`.

### D3. The light layer rebuilt whole, from the light prims, in path order

A light added, removed, or reshaped (an area light's `xform` or shape) rebuilds the
whole layer:
1. Take the provenance's light paths, plus the added ones and minus the removed ones,
   in path order.
2. Run `emit_light_layer` over a scratch `WorldBuilder` whose first id is `G`.
3. Judge and install the links (D2).
4. Apply `hide_infinite_from_camera` when the domes are not seen by the camera.
5. Rebuild `Provenance::lights` and its emitter runs.
6. Replace the scene's `LightList` and the world's layer (`World::replace_light_layer`).

`Renderer::new` then builds the light selection, as it does after every update.

Today's per-light replace path (`Rebuilding::light`, `LightList::check_replace`) stays.
It is the cheaper case of a parameter edit and does not touch the layer.

*Light id ordering.* Path order is kept (D3 of `moho-hydra-parity`). Rebuilding the
layer whole costs O(L) light reads (dome textures come from the kept assets),
O(E log E) for the segment, and O(L log L) for a tree selection. That is milliseconds
for the hundreds or low thousands of lights a shot holds. Rebuilding whole rather than
inserting one keeps a single code path that a fresh commit also runs.

*Guards* (any one re-commits, and the update reports `full`):
- an active render pass (`ctx.pass` flags emitters as it meets them);
- the scene's warnings name a light or a link (`light.*`, `light_link.*`);
- the rebuild raises a warning a fresh commit could report differently (the
  `unwarned` / `same_warnings` rules of `update.rs`);
- `CRUST_SCENE_DELTAS=0`.

The warnings a fresh commit raises about lights are raised in the light layer's order,
after the walk's warnings. Today they interleave with the walk's, so this is part of
D1's one-time move, and `Scene::warnings` order is pinned by the bitwise tests.

### D4. Settings and camera edits: what scales with the scene

From the code, without a profile, the settings path does O(scene) work in these places.
Each is checked by the profile in task 3.1 and fixed in this order.

| # | where | what grows | fix |
|---|---|---|---|
| S1 | `refresh_settings` → `HostFacts::invisible_overrides()` (`usd_import/stage_facts.rs`) | walks every spec path of the strongest layer on each refresh. The fixture authors every mesh in its root layer, which is a strong candidate for its linear growth (1.6 ms at 1k, 20 ms at 10k) | read once at the commit into `Provenance::stage` and reused. It only stamps the image (`Scene::source`). Changing it needs a visibility `over`, which dirties a prim's `visibility` and is not a `settings` batch |
| S2 | `Held::of` (`delegate/mod.rs`) builds `material_of_slot` from every provenance slot on every update | O(material slots), in `restart` | built on the first `pick` after a commit and cached until the next. In-place updates never move a slot's material path |
| S3 | `Held::of` → `Renderer::new` → `select_lights`, then `launch` → `reconfigure` → `select_lights` again | two selection builds per restart; under `learned`, two pre-passes | one build per restart, with the launch settings (`retune` where the key matches, or `Renderer::new` given the final settings) |
| S4 | `Held::into_scene` / `Held::of` reassemble the scene and build an empty `World` per update | allocation, small | measured; kept unless it shows |
| S5 | `read_index` → `import_render_products(…, Some((path, host)))` → `HostFacts::authoring_layer_dir` | per kernel asset path, a layer-stack query | measured on Moana |
| S6 | reads through the delegate's filters after a dirty (light linking at phase 4, dependency forwarding) | lazy recomputation in a Moho filter, if any | measured. If found, it is a Moho gap (§ Moho gaps) |
| S7 | `Delegate::update` → `assets.revalidate()` | one `stat` per kept file, before the split starts, so it is outside today's figures | timed into the split as `revalidate`. Kept, since the spec requires it. A cheaper policy is Q5 |

A test (task 3.6) wraps the delegate's index in a counting scene index and asserts that
a camera edit reads only the prims the "A settings update reads only the settings and
cameras" requirement lists. That pins S1-type walks from coming back. The update
split (`UpdateSplit`) gains `revalidate`, so the benchmark's columns sum to the
median.

### D5. Shared and instanced meshes reshaped through the commit's own readers

`Provenance` gains, per distinct mesh slot:
- its readers: direct placements (`geom_id`) and prototype parts (the prototype key of
  `ImportCaches::protos` and the part index);
- per prototype key, its parts' layout (slot count, masks, material `Signature`s) and
  the top-level `geom_id`s of every placement of each part.

A reshape (`mesh`, `primvars` or points/normals dirtied on a `Mesh` prim) is classified
`geometry` under the requirement's conditions and applied in one of three ways:

1. **Placed once, baked**: today's path (`Rebuilding::placed_mesh`).
2. **A direct mesh shared with other direct placements.** The commit interned them by
   content key. After the edit, a fresh commit gives the edited prim its own slot, and
   the remaining placements keep the old one.
   - `flush_meshes`'s decision is run again for both slots with their new placement
     counts, through the same function, factored out of `flush_meshes`.
   - A slot that drops to one placement becomes baked, so a placement can go from
     instance to baked. Each affected placement gets a `GeometryEdit` at its own id.
   - Slot ids are internal to the provenance, and geometry ids do not move: they are
     reserved at the walk.
3. **A prototype's mesh** (a native instance's propagated prototype, or a point
   instancer's prototype copy, keyed by Hydra prototype path).
   - The prototype's parts are rebuilt by `prototype_parts` / `build_parts`, with fresh
     caches over the index.
   - The rebuild must give the same part count, slot counts, masks and material
     signatures.
   - Each placement of each part then gets a `GeometryEdit` that carries the new
     `Arc<RtScene>` and the parts' side tables (face map, UV map, primvars).
   - `World::replace_geometry` re-commits the top level once.

Refused, re-committing:
- adaptive subdivision;
- a displaced or moving mesh;
- a nested instancer part (`nested_instancer_parts`);
- a slot read by both direct placements and a prototype, or by two prototypes;
- a new key equal to an existing slot's (a fresh commit would merge them);
- a part layout that changes.

Cost: O(the edited mesh + the prototype's parts + its placements), plus the top-level
tree rebuild every geometry edit already pays (Non-Goals).

*Alternative.* Keep the old key's slot shared and patch only the edited prim's BLAS.
That is wrong whenever a fresh commit would bake, so it was rejected.

### D6. Memory over re-commits: measure by site, then fix

Measurement comes first (tasks 2.6 and 8.4):
- The benchmark prints resident memory (`crust_core::current_memory_bytes`) and the peak
  (`peak_memory_bytes`) after setup and after each edit.
- A `--recommit K` loop forces `full` updates on the fixture.
- One run under heaptrack attributes the retained bytes to allocation sites.
- A run with `MALLOC_ARENA_MAX=2` separates allocator retention from live data. That is
  an environment variable, not code.

The suspects, in the order they are checked:
- **M1. Two scenes at once.** `SceneDelegate::apply` holds the old `Scene` (world,
  kept inputs, light list, provenance) across `self.commit`. A re-commit's peak is
  therefore old + new + the commit transient.
  - Fix: decide the class first. When it re-commits, drop the scene before reading the
    index.
  - "A re-commit holds one scene" pins it with a weak reference to the old world.
  - A failed commit leaves no scene. The notices stay pending, and the next update
    commits afresh. `Delegate` reports the error and holds nothing to render.
- **M2. Kept assets never swept on a re-commit.** `Delegate::update` sweeps only when
  there is no scene. A re-commit leaves every decode the new commit no longer asks for.
  Fix: sweep after every commit from scratch.
- **M3. Allocator retention.** glibc keeps freed arenas of the rayon threads that built
  the old BVH. If the `MALLOC_ARENA_MAX` run closes most of the gap, the fix is an
  allocator choice. That would be a dependency carrying `unsafe`, which is a project
  decision (Q4) and is not taken here.
- **M4. The kept inputs** (`WorldBuilder::keep_inputs`) cost the baked triangles a
  second time. This is steady, not growth, and is reported only. With D1 the emitters'
  inputs are the light layer's.
- **M5. Moho caches** that grow with each full re-read (stage scene index prim caches,
  flattening caches). Attributed by heaptrack. If found, it is a Moho gap.

Target: the resident memory after the third re-commit is within 5% of the memory after
the first, on the 10k fixture and on Moana. The peak during a re-commit is at most one
scene plus the commit transient above the steady state. Both are reported.

### D7. The benchmark and its targets

`crates/crust-render/examples/update_latency.rs` gains:
- the edits that the "Update latency is reported, not gated" requirement lists;
- a fixture extension: `--instanced P×M`, a native-instanced prototype mesh placed M
  times and a point instancer placing it P times;
- the memory columns;
- `--recommit K`;
- the `revalidate` split.

The added light's removal is now a timed edit.

Targets (reported against, never gated). *Crust's share* is stop + commit + restart.

| edit | fixture 10k, Crust's share | Moana, Crust's share |
|---|---|---|
| camera `focalLength` / transform | ≤ 1 ms, and within 1.5× of the 1k fixture | ≤ 5 ms |
| add or remove a light, trivial collections | ≤ 5 ms | ≤ 50 ms |
| add a light linked to one prim | ≤ 20 ms | ≤ 1 s (re-judge over the island's receivers and patch) |
| area light move | ≤ 5 ms | ≤ 50 ms |
| prototype mesh reshape | reported; bounded by the top-level rebuild | reported |
| memory | after 3 re-commits ≤ 1.05 × after the first | same |

End to end, Moana's light edits stay bounded by Moho's share: authoring 0.18–0.40 s,
and `apply_pending_updates` 0.25 s, or 3.58 s for the linked light. This change cannot
move it (§ Moho gaps). The figures go into `openspec/specs/render-delegate/design.md`
§ Update latency. The Moana runs use the protocol already recorded there. With light
edits in place, 10 runs become affordable.

### D8. The class table follows the new in-place cases

In `commit::classify`:
- an added or removed light no longer sets `recommit`. It fills `change.layer`, a new
  field: the light paths added and removed;
- `categories` / `instanceCategories` fill `change.categories`;
- a dirtied `Mesh` whose `reshapable` fails only for sharing goes to `change.reshaped`
  with its D5 case, decided from the provenance.

The table in the render-delegate design record is updated:
- the "re-committed" row shrinks to the guards;
- "a shared or instanced mesh's points or normals" leaves the `full` row.

`recommit` stays for the guards a batch alone can decide: an active pass.

## Moho gaps

None blocks this change. They are listed for Moho:
- **G1.** Adding a linked light on Moana costs 3.58 s in `apply_pending_updates`
  (Moho's own figure: 2.94 s), and 0.40 s of authoring. Moho's light-linking filter
  re-evaluates collections over the island. After this change it dominates the
  end-to-end time of a linked light edit.
- **G2** (conditional on S6). Lazy work in a renderer filter that a camera or settings
  read triggers.
- **G3** (conditional on task 7.6). A dirty notice on a prototype's prim must reach the
  propagated Hydra prototype paths the delegate keys parts by. If Moho's prototype
  propagation drops it, D5 case 3 cannot see the edit.
- **G4** (conditional on M5). Cache growth across full re-reads.

## Risks / Trade-offs

- **[Throughput]** A second segment traversal per ray where area lights exist.
  → Measured (D1). If the regression is above what the user accepts (Q3), fall back to
  inline emitters and a top-level rebuild per light change: still correct, seconds on
  Moana instead of milliseconds.
- **[Images move once]** Emitter ids and exact ties.
  → The goldens are checked, and moved ones shown to be noise (D1). An unlinked scene
  with no area light is bit-identical.
- **[Masks patched in leaves]** A future build that reads masks (for example,
  mask-aware packet grouping) would make a patched scene differ from a fresh one.
  → The layout-independence test of the intersection-kernel requirement fails first.
- **[Warnings order]** Light warnings move after the walk's.
  → Part of the one-time move. The bitwise tests compare warnings.
- **[Provenance grows]** Link record, per-slot readers, prototype placements:
  O(emitting prims + placements of prototypes).
  → Reported in the benchmark's memory columns. On Moana, placements are the
  instances the world already holds one `Geometry::Instance` for.
- **[One more pair]** The light layer ↔ its emission in a fresh commit, and judge ↔
  install on a builder and on a world.
  → Each has one implementation, and `docs/architecture.md` § Invariants names it.

## Migration Plan

- Archive order: `moho-hydra-parity` (PR #422) first, then this change, whose
  `render-delegate` deltas modify its requirements.
- Phases land in task order, each green on the three checks:
  1. benchmark and measurement;
  2. settings path;
  3. kernel segments and mask rewrite;
  4. light layer;
  5. links in place;
  6. shared reshapes;
  7. memory;
  8. benchmark run and record.
  The kernel phase is behaviour-neutral (one segment) and can land first.
- Goldens are re-recorded once, after the light layer, with the noise evidence in the
  design record.
- Rollback: revert the merge. `CRUST_SCENE_DELTAS=0` already gives a `full` update for
  every edit.

## Decisions awaiting the user

These are the defaults this plan takes. Each says what would change if the user
chooses otherwise.

- **Q1. Bitwise equality.** Default: **kept** (D1 changes the fresh commit's layout so
  that a light change is local).
  - Option **R**: relax it for the light layer. Append-only light indices and emitter
    ids. The in-place scene equals a fresh commit *up to a permutation* of light indices
    and emitter ids, which a test pins by comparing the canonicalised light set and the
    render's statistics (relative MSE falling as 1/√N).
    - It buys little here: the permutation is not what costs time, the tree rebuild is.
    - It loses the per-class bitwise pair that has caught every class-table bug so far.
  - Option **R′**: relax it for geometry edits only. Refit the top-level tree instead of
    rebuilding it, which makes moves and reshapes O(changed). That is a separate change
    (Non-Goals). It is the one relaxation that would buy a large factor on Moana's
    geometry edits.
- **Q2. Light id ordering.** Default: **path order**, the layer rebuilt whole (D3):
  O(L) per light edit. Insertion order needs Q1-R. A scene with tens of thousands of
  lights would pay milliseconds to tens of milliseconds per light edit.
- **Q3. Throughput budget for the emitter segment.** Default: report the measured
  regression and accept it if it is at most 2% on `bench_scenes.sh`'s area-lit scenes.
  Above that, the user decides between D1 and the inline fallback.
- **Q4. Allocator.** If M3 is the growth, the fix is mimalloc or jemalloc, a dependency
  with `unsafe` (a project decision by CLAUDE.md). Default: report, don't adopt.
- **Q5. `revalidate` per update.** Default: keep one `stat` per kept file per update,
  as the spec requires. If ALab shows it in milliseconds, the user may accept a cheaper
  policy, such as revalidating on a timer or only on a host request.

## Open Questions

- Whether `Held::into_scene` / `Held::of`'s reassembly (S4) is measurable at all. It
  is kept unless the profile shows it.
