## Context

See proposal.md for why. This section records the state the plan starts from: Crust at
`dacc6475`, Moho at `92c8b84`, openusd at `68a867a`.

**Crust's importer** (`crates/crust-core/src/scene/usd_import/`, about 23k lines).
`load_scene` works in three stages.

- **The index stage.** `load_scene` first opens a `LoadNone` index stage. `read_index`
  reads the render settings, products, vars, passes, the working colour space, the
  dicing camera and the shutter from it. It is kept until light links are resolved.
- **The traversal.** `traverse_stage` runs over one stage, or over one `LoadAll` stage
  per chunk when the stage has at least 4 top-level subtrees (`stream_roots`; each chunk
  is masked to its subtree plus the active pass prim). Each chunk is walked from the
  root by `traverse_into`, an explicit stack. That walk composes world matrices
  (`compose_with_parent`) and prunes abstract, inactive, `proxy`/`guide` and invisible
  prims, still walking invisible subtrees for cameras. It dispatches by typed schema.
  Geometry ids are handed out in traversal order. They key light links, pass flags and
  provenance.
- **After the last chunk.** Light links are resolved (`LightLinks::resolve`), meshes
  are flushed (the bake-versus-instance decision needs every chunk's placement counts),
  and the top-level BVH is committed.

| module | reads from openusd | Moho counterpart |
|---|---|---|
| `time` | thread-local time code (`EvalTimeScope`) | the index's time: `set_time`, sampled offsets |
| `settings`, `products`, `precomp` | `RenderSettings`/`Product`/`Var`, `crust:*` settings, `driver:parameters:*`, `ri:*`, `compute_render_spec`, filter prims named by relationship | `renderSettings` (flattened products and vars, `namespacedSettings`), scene globals, generic data on filter prims |
| `passes` | the active `RenderPass`: `matte`, `cameraVisibility`, opinions of `prune`/`renderVisibility` | `renderPass`, its `collections` (matte, cameraVisibility), generic relationships for the rest |
| `camera`, `adaptive` | `UsdGeomCamera`, `metersPerUnit` (and whether it is authored), conform policy | `camera` schema, scene globals (no authored flag: gap M3) |
| `xform` | `xformOpOrder` stacks, `XformQuery`, sample times over the shutter, `crust:motion:translate` | `xform` (local) and flattened world `xform`, sample times; generic data |
| `attrs` | `crust:rayMask`, `crust:subdivision*`, `crust:light:cameraVisible`, `primvars:ri:attributes:visibility:camera`, colour spaces | `__usdAttributes` (value, authored, colorSpace), `primvars` |
| `mesh` | topology, holes, subdivision tags, primvars (indices, interpolation, elementSize, colour space), normals precedence, `crust:displacementBound` | `mesh`, `primvars` (de-indexed reads), generic data |
| `shapes`, `strands` | `Sphere`, `BasisCurves`, `NurbsCurves`, `primvars:curve*`, widths | `sphere`, `basisCurves`, `nurbsCurves`, `primvars` |
| `implicit` | `Cube`, `Cone`, `Cylinder(_1)`, `Capsule(_1)`, `Plane`, tessellated by Crust | the shape schemas (not meshed by Moho, as Crust needs) |
| `materials`, `preview`, `mtlx_network` | binding resolution (`full` purpose, then all purposes; collection bindings), terminals, `info:id`, inputs and connections, `crust:openpbr`, `PxrDisneyBsdf`, `.mtlx` references read off layer specs, materials outside the chunk on a side stage, `prim_stack` and connections for provenance | `materialBindings` (resolved per purpose), `material` networks per context, asset values; no `.mtlx` arc (openusd has no MaterialX format, see `mtlx-format-plugin`) |
| `lights`, `light_links` | every UsdLux type, shaping, IES, dome texture and pole, `crust:light:*`; link collections via `compute_membership_query`, read on the index stage or the chunk | `light` (open container), `collections` (complete membership expressions), `categories` (per chunk only: gap M1) |
| `volume` | `crust:volume:*` on any prim; a material's volume terminal | generic data; `material` volume terminal |
| `instancing` | `is_instance`/`prototype` (`/__Prototype_N`, epoch-scoped), placements counted per subtree, raw `PointInstancer` arrays with per-index warnings | `instancer` prims, `instancerTopology`, `hydra:instance*` primvars, stable prototype paths under `MohoNiPropagatedPrototypes` / `ForInstancer<hash>` |
| `mod.rs` (stage facts) | `layer_identifiers`, `layer_stack`, session/root layer specs (`invisible_overrides`), `has_authored_time_code_range` | none: not imaging data (D7) |
| `live` | rebuilds one material, light, or the settings on a masked stage through the import's functions | the same functions, over an index of that stage (D8) |
| `listing` | `crust ls` | unchanged (not an import) |

**Live edits today** (`crate::live`, `usd_import/live.rs`). The MCP session and the
host session diff layers. A classifier, fed by the scene's `Provenance` (arc sources
from `prim_stack`, the prims a material reads, per-light and per-mesh origins), decides
whether an edit can be rebuilt in place. A rebuild calls the import's own functions on a
stage masked to the edited prim. The rule "an MCP edit applied in place equals a full
re-import" is pinned bitwise.

**Moho** gives `StageSceneIndex` (`new`, `set_stage`, `set_time`,
`apply_pending_updates`, `set_include_unloaded_prims`, and `stage()`),
`moho::chain::standard`, the public filters (so a chain can be assembled from them),
`MergingSceneIndex` (`add_input_scene`, `insert_input_scene`, `remove_input_scene`),
typed schemas, and `__usdAttributes`/`__usdRelationships`. Its contract is
`Moho/openspec/specs/*/spec.md`. Data sources are `Rc` and confined to one thread;
values are `Send`. Moho's `moho-spike` is the streaming memory benchmark: on Moana,
strategy (B), a masked stage per chunk, peaks at 1.97 GiB.

**Constraints.**
- Moho pins openusd by `rev`. crust-render patches by `branch = "main"`, which locks to
  the same commit but is a different Cargo source.
- Moho is public. It is consumed by git rev, and a sibling path patch is optional for
  co-development.
- crust-core stays `deny(unsafe_code)`. Moho is `forbid`.
- The import is single-threaded (`time.rs`). Only `MeshArena::commit_slots` uses rayon,
  on arrays already read.

## Goals / Non-Goals

**Goals:**
- An index route that produces, for every scene, the `Scene` the direct route produces:
  the same geometry in the same order, the same materials, lights, camera, settings,
  AOVs, warnings and provenance.
- Migrate one family at a time, with the parity check green after each move.
- Keep the direct route's memory bound on streamed scenes, and the import time within a
  stated bound.
- One openusd copy in the build, checked rather than assumed.
- An importer shaped for S3: readers are functions of (index, path) that feed Crust's
  builders, so a later change can call them for dirtied prims.

**Non-Goals:**
- S3: dirty notices driving in-place updates, observers on the index, retiring the
  layer-diff classifier, or the session keeping an index alive between applies.
- Removing the direct importer. A follow-up change does that after the default flips.
- Behaviour changes that Hydra semantics would bring, such as primvar inheritance,
  `includedPurposes` (that is `payload-and-purpose-view`'s) or binding purposes beyond
  today's.
- Changing Moho. Gaps are filed against Moho (M1 to M7) and planned around openly.
- `crust ls` (`listing.rs`) and the MCP session's authoring stage. Neither imports a
  scene.

## Decisions

### D1. One openusd: the patch pins Moho's rev

`[patch.crates-io]` changes from `branch = "main"` to
`rev = "68a867a69f3fb429a30f9e1155a0a30a3d2cdf9f"` for both openusd and openusd-schemas.
`[workspace.dependencies]` gains
`moho = { git = "https://github.com/doubleailes/Moho", rev = "92c8b847822823f796b81b85452c3c400d77e5ec" }`.
Cargo identifies a git source by URL plus reference, so `?branch=main` and `?rev=…` are
two packages even at one commit. Moho's `openusd = { git, rev }` and crust's patched
crates.io `openusd 0.8` resolve to one package only when both name the same rev. Three
checks guard it:
- `cargo tree -i openusd` and `cargo tree -i openusd-schemas` each show one package;
- `cargo tree -d` lists no openusd crate;
- a compile-time test in crust-core passes a `moho::openusd::usd::Stage` where an
  `openusd::usd::Stage` is expected, and the same for one openusd-schemas type. It stops
  compiling if two copies ever appear.

*Alternatives:* `branch = "main"` in Moho. Rejected: Moho's rule is an exact rev, and
`main` moves. A `git =` entry in `[workspace.dependencies]` in place of the patch.
Rejected: the patch keeps the `0.8` requirement, so dropping it is still the whole
revert.

The bump order becomes Moho, then crust-render, then Mantle. `cargo update -p openusd`
stops being the way to move. A bump edits both revs: Moho's rev in the `moho` entry and
the openusd rev in the patch. `CLAUDE.md` says so.

### D2. Use openusd through Moho's re-exports at the index boundary

New index-route code names openusd types as `moho::openusd` and
`moho::openusd_schemas`. The workspace's own `openusd` and `openusd-schemas`
dependencies stay. crust-core needs features Moho does not turn on (`mmap`, the `skel`,
`proc` and `media` schema families), crust-render needs `serde`, and the direct route
uses them. Feature unification gives both names the same crate (D1's test proves it). A
reader of the index code can see which types cross the Moho boundary.

### D3. The route switch

`Config` gains `usd_import: ImportRoute { Direct, Index }`, read from
`CRUST_USD_IMPORT` (`direct` or `index`; any other value warns once and keeps the
default). `UsdImportOptions` gains `route: Option<ImportRoute>`, which overrides the
environment for one import. Tests run in parallel in one process, so they cannot use the
environment. Following the switch convention, the off side is today's importer: `direct`
is the default until task group 15 flips it. `load_scene` dispatches once. Everything
before the traversal that does not read the stage (option checks, warnings scope, stats)
is shared.

### D4. Index layout: S0's merged index, the consumer deciding what is in

- **Single stage** (fewer than 4 subtrees, or `CRUST_STREAM_IMPORT=0`): one
  `StageSceneIndex` over a `LoadAll` stage opened with Crust's `stage_builder()`, which
  keeps the schema registry and the `mmap` resolver. The chain is built over it (D6).
- **Streamed**: a `MergingSceneIndex` with:
  - the **scene input**, weakest and permanent: a chain over the `LoadNone` index stage
    with `set_include_unloaded_prims(true)`. It replaces `read_index`'s direct reads of
    settings, products, passes and cameras outside payloads.
  - one **chunk input** at a time: a chain over a `LoadAll` stage masked to the chunk
    (and the active pass prim, as today), added with `add_input_scene(chunk, root)`,
    walked, then removed with `remove_input_scene`, and its stage dropped. That is S0's
    strategy (B) and its memory bound.
  - a **side input** when a chunk binds a material that neither the chunk nor the scene
    input holds. It is a chain over a stage masked to the out-of-chunk binding targets
    (and the prims their networks connect to), payloads loaded. It is grown and reopened
    under the same rules as today's `OutsideMaterials`, and removed after the last
    chunk. This is a consumer deciding what is loaded, built from Moho's own pieces.
    Moho has not specified it, so M2 asks Moho to.
- Crust keeps opening every stage itself and hands it to `set_stage`. Moho never loads
  or unloads.

*Alternative:* one chain per chunk, walked without a merging index. Rejected. S0 decided
on the merged index, the scene input already holds what `read_index` reads, and a
material in the scene input must be reachable from a chunk's prims by path.

### D5. The traversal: Crust walks, in the direct route's order

The index route has its own walk, from the absolute root, over the index's `children`,
with the direct route's stack discipline (push in order, pop last-first). Geometry ids,
light-link runs, pass flags and provenance depend on visit order, and bitwise image
parity depends on geometry order. While streaming, a chunk is walked from the root down
to and through that chunk's root only. Below the chunk root the merged children come
from the chunk input in stage order (the scene input's children there are a subset).
Outside the chunk, only the ancestors are visited, as the masked stage gives today.
`MergingSceneIndex::children` returns the union in input order, not namespace order
(M7), so the walk never relies on it above a chunk root.

- World transforms come from the flattened `xform`. Visibility and purpose come from
  the flattened data, applying today's pruning: abstract and inactive prims are absent
  from the index, `proxy`/`guide` prune, invisible prims prune but are still walked for
  cameras. Shutter samples come from `sample_times` over the shutter offsets.
- Dispatch is by Hydra prim type (`mesh`, `sphere`, `basisCurves`, `instancer`, the
  light types, `camera`, ...). A prim with `crust:volume:type` in its generic data
  dispatches as a volume first, as today. A geometry prim that no branch reads is
  counted for `geom.unsupported_type` from its type, as today.

### D6. Migration by family, with a bridge that is deleted at the end

The index route starts with every family on a **bridge**: for a prim at a stage path,
the bridge asks the input that holds it (the current chunk, the side input or the scene
input) for its `StageSceneIndex::stage()` and hands the existing reader the `usd::Prim`
at that path, at the same time code. Parity therefore holds from the first commit of the
route. Each family then moves on its own to an **index reader**: a function of (scene
index, path) that reads schemas and generic data and calls the same builders the direct
reader calls. The move is accepted when `import-parity` is green.

The bridge cannot reach a prim that exists only in Moho's namespace (re-rooted
prototypes). So until instancing moves, the index route builds its chain without the two
instancing filters:
```
StageSceneIndex -> Flattening -> MaterialBindingResolving -> LightLinking -> RenderSettingsFlattening
```
An instance or `PointInstancer` prim goes through the bridge to today's
`emit_native_instance` and `emit_point_instancer`, and that holds the prototype epoch
scoping. Instancing moves last. With it the chain becomes `moho::chain::standard`, and
prototypes are keyed by Moho's stable prototype paths. Those paths need no epoch,
because they agree across chunks by construction. The epoch stays for the direct route.
After that move the bridge has no callers, and it is deleted in the same task group.

Family order follows the data each one needs:
1. settings, products, precomp, passes, camera (the camera needs the aspect);
2. transforms and motion;
3. meshes (primvars, subdivision, adaptive level, displacement bound);
4. implicit shapes and spheres;
5. curves;
6. materials;
7. lights and light links;
8. volumes;
9. instancing.

*Alternative:* write the whole index importer, then compare. Rejected. 23k lines with
no intermediate parity would make a difference undiagnosable, and Mantle's design asks
for one module at a time.

### D7. What is not imaging data is read from the stage

Some facts are not scene description: the layers a scene composed (`Scene::layers`,
`Source::layer_stack`), session/root-layer `invisible` overrides, whether the time range
or `metersPerUnit` is authored, a material's composition arcs (`prim_stack`) for
provenance, and a `.mtlx` reference read off layer specs. The index route reads these
from `StageSceneIndex::stage()`, behind one module (`index/stage_facts.rs`), each with a
comment naming why it is not a schema read. Two of them are Moho gaps that Moho can
close (M3 authored-ness, M4 prototype source paths). Two are pending other work (the
`.mtlx` arc moves to `mtlx-format-plugin`; `prim_stack` provenance goes with S3's
classifier). The rest (layers) are a host's business, not a scene index's. This is the
documented escape hatch, not a silent workaround.

### D8. In-place rebuilds move with their family

`usd_import/live.rs` rebuilds a material, a light, or the settings and camera on a
masked stage through the import's functions. When a family moves, its rebuild builds a
`StageSceneIndex` over that masked stage and calls the same index reader, under the same
route as the scene's import (`StageFacts::options` carries the route). This keeps the
pair "an in-place update equals a full re-import" true on both routes, with
`crust-core/tests/live.rs` run under both. The classifier and `Provenance` are S3's to
replace. They keep their inputs, which D7 reads from the stage where Moho has no
counterpart.

### D9. Light links: Crust keeps deciding after the last chunk

Moho's `LightLinkingSceneIndex` sits in each input's chain. It knows only the lights of
its own input, so a receiver in chunk B gets no category for a light in chunk A or in
the scene input (M1). The direct route already defers link decisions to after the last
chunk, judging recorded paths. The index route keeps that structure. It reads each
light's `collections/lightLink` and `collections/shadowLink` (Moho's complete membership
expressions), judges every recorded receiver path after the last chunk, and does not
read `categories`. On a single stage the two agree, and a parity test asserts it there,
so the M1 fix can later switch Crust to `categories` with a test already in place.
Today's choice between the index stage and the chunk for a light's link opinions
becomes: the merged data of the light (chunk opinions over scene input), with the same
warning when a nested collection is not composed.

### D10. Time: the index owns it

Every `StageSceneIndex` the import builds is set to the import's time
(`TimeCode::default()` for `None`, the authored default, as today). The index route
reads sampled data at offset 0, and at the shutter offsets for motion. `EvalTimeScope`
stays only for bridged readers and the direct route. It goes from the index route with
the bridge. `LiveSession::set_time` (`live-session-time`) keeps its behaviour: a time
change is a full import.

### D11. What parity compares

See `specs/import-parity`. In short:
- a structural `Scene` digest, test-only in crust-core, over geometry in id order,
  materials by resolved parameters, the light list, camera, settings, AOV request,
  warnings in order and provenance;
- the `crust check --json` report minus timings;
- a 16 spp EXR compared bitwise with `crust diff`;
- for ALab and Moana, `--stats` peak memory and import time, compared between routes in
  one `bench_ab.sh`-style interleaved run.

A difference that is correct (for example a Hydra semantic Crust should adopt) is not
let through. It is recorded under Open Questions and left for its own behavioural
change.

### Moho-side follow-ups (filed in the Moho repo, not done here)

- **M1. Cross-input light linking.** In a merged streamed index, each input's
  `LightLinkingSceneIndex` sees only its own lights, so a chunk's geometry lacks
  categories for lights in other inputs. Ask: resolve linking after the merge (a
  scene-wide linking filter over the `MergingSceneIndex`), with a streaming test whose
  fixture links across chunks. Crust's interim is D9.
- **M2. Prims outside the loaded chunks.** S0 left "how a consumer asks for a material
  bound from another chunk" to S1, and S1's specs do not say. Ask: specify and test the
  side-input pattern of D4 (a chain over a stage masked to out-of-chunk targets, as an
  extra merging input), or give `MergingSceneIndex` a documented hook.
- **M3. Authored-ness in the scene globals.** `metersPerUnit` and the time range are
  given as values only. Crust warns differently when they are unauthored. Ask: an
  `authored` flag beside each, as `__usdAttributes` already has.
- **M4. Source paths for prototype prims.** Re-rooted prototype prims
  (`MohoNiPropagatedPrototypes/...`, `ForInstancer<hash>`) do not name the USD prim
  they come from. Crust's warnings and provenance name prototype-internal prims by their
  stage path. Ask: expose the source prim path (as `__usdPrimInfo` does for instances).
  Until then, warning paths inside prototypes are a listed parity exception for the
  instancing family only, normalised by the parity tooling.
- **M5. An index strategy in `moho-spike`.** S0 measured raw openusd reads. Ask: a
  strategy (D) that does (B) through `MergingSceneIndex` and standard chains, so Moho's
  own memory overhead is measured against (B) on Moana.
- **M6. Pass collections.** `renderPass` exposes `matte` and `cameraVisibility` only.
  Crust reads `prune` and `renderVisibility` opinions to warn that it does not honour
  them. Generic relationships cover it now. Ask: expose them as `collections` for
  completeness.
- **M7. Merged children in namespace order.** `MergingSceneIndex::children` returns the
  union in input order. D5 avoids relying on it. Ask: namespace order, or document the
  current order.

## Risks / Trade-offs

- **[Visit order differs, so geometry ids differ, so the image differs]** → D5 walks
  per chunk with the direct route's stack discipline. The digest compares geometry in id
  order, so a reorder is caught at the first family that emits geometry, not at the end.
- **[Prototype namespace mismatch]** (Moho re-roots prototypes by content and binding;
  Crust keys `/__Prototype_N` by epoch) → instancing moves last, on the standard chain.
  Moho groups instances by binding, while Crust's `MeshKey` deduplicates by geometry
  and material. These are different keys, so the bake-versus-instance decision is
  re-checked by the digest, including placement counts per subtree (adaptive
  subdivision).
- **[Hydra preprocessing hides what Crust warns about]** (a `PointInstancer`'s
  out-of-range `protoIndices`, `inactiveIds` already removed from `instancerTopology`;
  unauthored-ness) → the index reader reads the raw arrays from `__usdAttributes` where a
  warning depends on them, as the generic data exists for.
- **[Inherited constant primvars]** Moho's flattened `primvars` include ancestors'
  constant primvars, which Crust does not read today → the index reader reads a prim's
  own constant primvars from its generic data, preserving today's behaviour. Whether to
  adopt USD's inheritance is an Open Question.
- **[Import time overhead]** (layers of `Rc` data sources, string-keyed containers) →
  the ALab and Moana gates bound it (`import-parity`). Hot arrays (points, indices,
  primvars) are read once as shared `sdf::Value` arrays, never element by element
  through data sources.
- **[Memory]** (the scene input keeps a `LoadNone` stage alive for the whole traversal,
  as the direct route already keeps its index stage until light links) → it is dropped
  at the same point, and Moana's peak is gated. Each chunk input is removed and its
  stage dropped before the next opens.
- **[openusd bump friction]** (three repos pinned to one rev) → D1's checks fail loudly.
  The bump order is written in `CLAUDE.md`.
- **[Pending changes touch the same code]** (`payload-and-purpose-view` adds
  `includedPurposes`, binding purposes and load-set flags; `mtlx-format-plugin` moves
  the `.mtlx` arc into composition) → whichever lands first, the other's tasks must
  cover both routes. The parity tests cover new behaviour automatically, because they
  run every import test under both routes.
- **[Thread confinement]** (Moho data sources are `!Send`) → the import is already
  single-threaded. `commit_slots` takes arrays, never data sources. Values crossing to
  rayon are `sdf::Value`/`Vec`, which are `Send`.

## Migration Plan

1. Dependencies (D1, D2) land alone. No behaviour change. CI is green with one openusd.
2. The route switch, the parity harness and the index skeleton land, with every family
   bridged. `CRUST_USD_IMPORT=index` equals `direct` from here on.
3. One task group per family, each merged only with parity green on the samples and
   fixtures. ALab and Moana are run at the materials, lights and instancing milestones
   and at the end.
4. The bridge is deleted with the last family (instancing).
5. With parity on ALab and Moana within the memory and time bounds, the default flips to
   `index`. hdCrust's `compare.sh` and `interaction.sh` run on it. The design record
   and docs are updated.
6. Rollback at any point is `CRUST_USD_IMPORT=direct`. After the flip, reverting that
   one commit restores the old default. The direct route is removed in a later change,
   not here.

## Open Questions

- **Primvar inheritance.** USD (and Hydra's flattening) makes an ancestor's constant
  primvar visible to descendants, and Crust reads a prim's own only. Adopting
  inheritance is a behavioural change to `usd-scene-import`, proposed separately after
  the flip. It does not change this plan: parity keeps today's behaviour.
- **Light links on `categories`.** Once M1 lands, does Crust read Moho's categories
  instead of judging expressions itself (D9)? It is deferrable, because the
  single-stage agreement test is already there to switch on.
- **When the direct route goes.** Mantle's design says "until parity holds, then goes".
  This change flips the default. The removal (with `EvalTimeScope`, the epoch and the
  direct readers) is a follow-up change, timed after S3 has started on the index route.
