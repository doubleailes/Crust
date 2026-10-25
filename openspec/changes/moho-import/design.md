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
| `settings`, `products`, `precomp` | `RenderSettings`/`Product`/`Var`, `crust:*` settings, `driver:parameters:*`, `ri:*`, `compute_render_spec`, filter prims named by relationship | `renderSettings` (flattened products and vars, `namespacedSettings`), scene globals and `__usdStageMetadata` (M3), generic data on filter prims |
| `passes` | the active `RenderPass`: `matte`, `cameraVisibility`, opinions of `prune`/`renderVisibility` | `renderPass`, its `collections` (`renderVisibility`, and `prune`, `matte`, `cameraVisibility` when applied: M6) |
| `camera`, `adaptive` | `UsdGeomCamera`, `metersPerUnit` (and whether it is authored), conform policy | `camera` schema, `__usdStageMetadata/metersPerUnit/{value, authored}` on the absolute root (M3) |
| `xform` | `xformOpOrder` stacks, `XformQuery`, sample times over the shutter, `crust:motion:translate` | `xform` (local) and flattened world `xform`, sample times; generic data |
| `attrs` | `crust:rayMask`, `crust:subdivision*`, `crust:light:cameraVisible`, `primvars:ri:attributes:visibility:camera`, colour spaces | `__usdAttributes` (value, authored, colorSpace), `primvars` |
| `mesh` | topology, holes, subdivision tags, primvars (indices, interpolation, elementSize, colour space), normals precedence, `crust:displacementBound` | `mesh`, `primvars` (de-indexed reads), generic data |
| `shapes`, `strands` | `Sphere`, `BasisCurves`, `NurbsCurves`, `primvars:curve*`, widths | `sphere`, `basisCurves`, `nurbsCurves`, `primvars` |
| `implicit` | `Cube`, `Cone`, `Cylinder(_1)`, `Capsule(_1)`, `Plane`, tessellated by Crust | the shape schemas (not meshed by Moho, as Crust needs) |
| `materials`, `preview`, `mtlx_network` | binding resolution (`full` purpose, then all purposes; collection bindings), terminals, `info:id`, inputs and connections, `crust:openpbr`, `PxrDisneyBsdf`, `.mtlx` references read off layer specs, materials outside the chunk on a side stage, `prim_stack` and connections for provenance | `materialBindings` (resolved per purpose), `material` networks per context, asset values; no `.mtlx` arc (openusd has no MaterialX format, see `mtlx-format-plugin`) |
| `lights`, `light_links` | every UsdLux type, shaping, IES, dome texture and pole, `crust:light:*`; link collections via `compute_membership_query`, read on the index stage or the chunk | `light` (open container), `collections` (complete membership expressions), `categories` (after the merge, `chain::merged`: M1) |
| `volume` | `crust:volume:*` on any prim; a material's volume terminal | generic data; `material` volume terminal |
| `instancing` | `is_instance`/`prototype` (`/__Prototype_N`, epoch-scoped), placements counted per subtree, raw `PointInstancer` arrays with per-index warnings | `instancer` prims, `instancerTopology`, `hydra:instance*` primvars, stable prototype paths under `MohoNiPropagatedPrototypes` / `ForInstancer<hash>`, `primOrigin/scenePath` naming each prototype prim's USD source (M4) |
| `mod.rs` (stage facts) | `layer_identifiers`, `layer_stack`, session/root layer specs (`invisible_overrides`), `has_authored_time_code_range` | the time range's authored-ness: `__usdStageMetadata` (M3); the rest none: not imaging data (D7) |
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
`moho::chain::standard`, `chain::input` (the standard chain without light linking) and
`chain::merged` (light linking over a merging index), `moho::streaming::unresolved`, the
public filters (so a chain can be assembled from them),
`MergingSceneIndex` (`add_input_scene`, `insert_input_scene`, `remove_input_scene`),
typed schemas, and `__usdAttributes`/`__usdRelationships`. Its contract is
`Moho/openspec/specs/*/spec.md`. Data sources are `Rc` and confined to one thread;
values are `Send`. Moho's `moho-spike` is the streaming memory benchmark: on Moana,
strategy (B), a masked stage per chunk, peaks at 1.97 GiB, and strategy (D) does (B)
through Moho (`scripts/streaming-spike.sh --with-d`).

**Moho's rev.** The plan was written while Moho's gaps M1 to M7 were open. Moho's change
`scene-index-gaps` closed them and merged into Moho's `main` at `fc391a5` (Moho-side
follow-ups, below); the workspace pins `moho` at that rev, and builds against it as CI
does, with no local patch.

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
- One import, through Moho, that produces for every scene the `Scene` the direct
  importer produced, as recorded in baselines before anything migrated: the same
  geometry in the same order, the same materials, lights, camera, settings, AOVs,
  warnings and provenance.
- Migrate one family at a time, with parity against the baselines green after each
  move.
- Delete the direct importer at the end of this change: its readers, the bridge,
  `EvalTimeScope` and the prototype epoch. Every host then imports through Moho.
- Keep the direct importer's memory bound on streamed scenes (one chunk's stage at a
  time), and measure memory and import time on ALab and Moana against its recorded
  baselines. As UsdImaging did when it moved onto scene indices, parity is the gate and
  performance is reported.
- One openusd copy in the build, checked rather than assumed.
- An importer shaped for S3: readers are functions of (index, path) that feed Crust's
  builders, so a later change can call them for dirtied prims.

**Non-Goals:**
- S3: dirty notices driving in-place updates, observers on the index, retiring the
  layer-diff classifier, or the session keeping an index alive between applies.
- Behaviour changes that Hydra semantics would bring, such as primvar inheritance,
  `includedPurposes` (that is `payload-and-purpose-view`'s) or binding purposes beyond
  today's.
- Changing Moho. The gaps the plan found (M1 to M7) are closed in Moho's own change
  `scene-index-gaps`; a new gap stops the work on that point and goes through Moho's
  OpenSpec flow, not a workaround here.
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
`proc` and `media` schema families), crust-render needs `serde`, and the direct importer
uses them. Feature unification gives both names the same crate (D1's test proves it). A
reader of the index code can see which types cross the Moho boundary.

### D3. One route, and baselines from today's importer

There is no route switch. The import moves to Moho in place, and the direct importer is
deleted at the end of this change (the user's decision: "the Crust importer can drop and
move to Moho"). A switch would only keep two readers alive to drift apart, which is what
the move exists to end.

What replaces the live A/B is a set of **baselines recorded from the direct importer**
before anything migrates (task group 2), kept for the whole change, as UsdImaging kept
its legacy route's baselines when it moved onto scene indices:
- a scene digest (D11) for every committed sample and import test fixture;
- the `crust check --json` report, timings excluded, for every committed sample;
- a hash of the 16 spp image's pixels (`--indirect-clamp 0`) for every committed sample
  and import test fixture;
- for ALab and Moana, the report, the EXR, and the peak memory and import time.

The small baselines are committed under `crust-core/tests/data/import_baselines/`. The
large-scene ones are kept in a local directory by `scripts/import_parity.sh record`
(EXRs of hundreds of megabytes are not committed), and their memory and time go into the
`usd-scene-import` design record. `CRUST_RECORD_BASELINES=1` (tests) and `record`
(script) regenerate them; after the first recording that is a behavioural change of its
own, never a way to accept a migration's difference.

Until a family moves, the transitional bridge (D6) keeps its direct reader running
under the index walk, so the tests stay green at every commit. The direct readers, the
bridge, `EvalTimeScope` and the prototype epoch are deleted together in the final code
group, once every family reads the index and parity holds on the samples, fixtures,
ALab and Moana.

### D4. Index layout: S0's merged index, the consumer deciding what is in

- **Single stage** (fewer than 4 subtrees, or `CRUST_STREAM_IMPORT=0`): one
  `StageSceneIndex` over a `LoadAll` stage opened with Crust's `stage_builder()`, which
  keeps the schema registry and the `mmap` resolver. The chain is built over it (D6).
- **Streamed**: `chain::merged` (light linking, M1) over a `MergingSceneIndex` whose
  inputs are each a `chain::input`:
  - the **scene input**, weakest and permanent: a chain over the `LoadNone` index stage
    with `set_include_unloaded_prims(true)`. It replaces `read_index`'s direct reads of
    settings, products, passes and cameras outside payloads.
  - one **chunk input** at a time: a chain over a `LoadAll` stage masked to the chunk
    (and the active pass prim, as today), added with `add_input_scene(chunk, root)`,
    walked, then removed with `remove_input_scene`, and its stage dropped. That is S0's
    strategy (B) and its memory bound.
  - a **side input** when a chunk binds a material that neither the chunk nor the scene
    input holds. `moho::streaming::unresolved(index)` lists those paths (the bound
    materials and the network nodes outside a material that no input holds, M2). The
    side input is a `chain::input` over a stage masked to them, payloads loaded,
    inserted weaker than the chunks and stronger than the scene input, as Moho's
    streaming pattern specifies. It is grown and reopened under the same rules as
    today's `OutsideMaterials` (at most `MAX_SIDE_STAGE_OPENS` per material) until
    nothing is unresolved, and removed after the last chunk. Moho lists what is missing;
    Crust decides what is loaded.
- Crust keeps opening every stage itself and hands it to `set_stage`. Moho never loads
  or unloads.

*Alternative:* one chain per chunk, walked without a merging index. Rejected. S0 decided
on the merged index, the scene input already holds what `read_index` reads, and a
material in the scene input must be reachable from a chunk's prims by path.

### D5. The traversal: Crust walks, in the direct importer's order

The index route has its own walk, from the absolute root, over the index's `children`,
with the direct importer's stack discipline (push in order, pop last-first). Geometry ids,
light-link runs, pass flags and provenance depend on visit order, and bitwise image
parity depends on geometry order. While streaming, a chunk is walked from the root down
to and through that chunk's root only. Below the chunk root the merged children come
from the chunk input in stage order (the scene input's children there are a subset).
Outside the chunk, only the ancestors are visited, as the masked stage gives today.
`MergingSceneIndex::children` returns the inputs' children strongest first, then the
prims implied by deeper roots (Moho specifies that order, M7). It is not the stage's
namespace order above a chunk root, so the walk never relies on it there.

- World transforms come from the flattened `xform`. Pruning applies today's rule from
  the index: abstract and inactive prims are absent from it, and each prim's own
  `purpose` and `visibility` (generic attribute data, not the flattened values: Hydra
  lets a child's authored purpose override an ancestor's `proxy`, where Crust prunes
  the subtree) prune `proxy`/`guide` and invisible subtrees, the latter still walked
  for cameras. Shutter samples come from `sample_times` over the shutter offsets.
- Dispatch is by Hydra prim type (`mesh`, `sphere`, `basisCurves`, `instancer`, the
  light types, `camera`, ...). A prim with `crust:volume:type` in its generic data
  dispatches as a volume first, as today. A geometry prim that no branch reads is
  counted for `geom.unsupported_type` from its type, as today.

### D6. Migration by family, with a transitional bridge

The bridge is a device of this change only: it exists so that the index walk replaces
the direct traversal at once while the tests stay green, and it goes with the direct
readers it calls (D3). The index route starts with every family on a **bridge**: for a prim at a stage path,
the bridge asks the input that holds it (the current chunk, the side input or the scene
input) for its `StageSceneIndex::stage()` and hands the existing reader the `usd::Prim`
at that path, at the same time code. Parity with the baselines therefore holds from the
first commit of the index walk. Each family then moves on its own to an **index reader**: a function of (scene
index, path) that reads schemas and generic data and calls the same builders the direct
reader calls. The move is accepted when `import-parity` is green against the unchanged baselines.

The bridge cannot reach a prim that exists only in Moho's namespace (re-rooted
prototypes). So until instancing moves, the index route builds each input's chain
without the two instancing filters, in `chain::input`'s order, with light linking over
the result as `chain::standard` (one stage, the stage as membership oracle) or
`chain::merged` (a merging index) puts it:
```
StageSceneIndex -> Flattening -> MaterialBindingResolving -> RenderSettingsFlattening   (per input)
  -> LightLinking                                                                      (over the input, or the merge)
```
An instance or `PointInstancer` prim goes through the bridge to today's
`emit_native_instance` and `emit_point_instancer`, and that holds the prototype epoch
scoping. Instancing moves last. With it the chains become `moho::chain::standard` (one stage) or
`chain::input` per input under `chain::merged` (streamed), and
prototypes are keyed by Moho's stable prototype paths. Those paths need no epoch,
because they agree across chunks by construction. After that move the bridge has no
callers, and it is deleted with the direct readers and the epoch (D3).

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
`Source::layer_stack`), session/root-layer `invisible` overrides, a material's
composition arcs (`prim_stack`) for provenance, and a `.mtlx` reference read off layer
specs. The index route reads these from `StageSceneIndex::stage()`, behind one module
(`index/stage_facts.rs`), each with a comment naming why it is not a schema read. Two
are pending other work (the `.mtlx` arc moves to `mtlx-format-plugin`; `prim_stack`
provenance goes with S3's classifier). The rest (layers, layer-spec overrides) are a
host's business, not a scene index's. This is the documented escape hatch, not a silent
workaround.

Whether the time range or `metersPerUnit` is authored is not among them any more. Moho
gives it on the absolute root as `__usdStageMetadata/<field>/{value, authored}` (M3), so
the index route reads it there, not from `stage_facts`. A prototype prim's USD source is
not either: every prim carries `primOrigin/scenePath` (M4).

### D8. In-place rebuilds move with their family

`usd_import/live.rs` rebuilds a material, a light, or the settings and camera on a
masked stage through the import's functions. When a family moves, its rebuild builds a
`StageSceneIndex` over that masked stage and calls the same index reader as the import.
This keeps the pair "an in-place update equals a full re-import" true, with
`crust-core/tests/live.rs` checking it at every step. The classifier and `Provenance` are S3's to
replace. They keep their inputs, which D7 reads from the stage where Moho has no
counterpart.

### D9. Light links: Crust keeps deciding after the last chunk

Moho runs light linking after the merge (`chain::merged`, M1), so a receiver in chunk B
gets the categories of a light in chunk A or in the scene input. But a receiver's
categories are only complete while every light's input is present, and an offline import
removes each chunk before the next is added: a receiver walked in chunk B cannot see a
light that only chunk D, not yet added, holds. The direct importer already defers link
decisions to after the last chunk, judging recorded paths. The index route keeps that
structure. It reads each light's `collections/lightLink` and `collections/shadowLink`
(Moho's complete membership expressions), judges every recorded receiver path after the
last chunk, and does not read `categories`. An agreement test asserts that Moho's
`categories` match Crust's answers, on a single stage (`chain::standard`) and on a
streamed fixture (`chain::merged` with every input present), so switching Crust to
`categories` is a tested option. Whether to switch is an open question (below).
Today's choice between the index stage and the chunk for a light's link opinions
becomes: the merged data of the light (chunk opinions over scene input), with the same
warning when a nested collection is not composed.

### D10. Time: the index owns it

Every `StageSceneIndex` the import builds is set to the import's time; with none, the
index's time is left unset, Moho's default time, which reads authored defaults as today. The index route
reads sampled data at offset 0, and at the shutter offsets for motion. `EvalTimeScope`
stays only for bridged readers, and it is deleted with them (D3); the `CLAUDE.md` rule
that the import reads its time from a thread-local goes with it. `LiveSession::set_time` (`live-session-time`) keeps its behaviour: a time
change is a full import.

### D11. What parity compares

See `specs/import-parity`. Every comparison is with the baselines recorded from the
direct importer (D3). In short:
- a structural `Scene` digest, test-only in crust-core, over geometry in id order,
  materials by resolved parameters, the light list, camera, settings, AOV request,
  warnings in order, provenance, layers and `Source`, as stable text so a difference is
  named, not just detected;
- the `crust check --json` report minus timings;
- the 16 spp image, by a hash of its pixels in the tests and bitwise with `crust diff`
  in `scripts/import_parity.sh`;
- for ALab and Moana, `--stats` peak memory and import time, the minimum of `-n` runs,
  against the baseline's, reported and not gated.

A difference that is correct (for example a Hydra semantic Crust should adopt) is not
let through. It is recorded under Open Questions and left for its own behavioural
change.

### Moho-side follow-ups (closed in Moho's `scene-index-gaps`)

Planning this change found seven gaps in Moho. Moho's change `scene-index-gaps` closed
all of them, with tests, and merged into Moho's `main` at `fc391a5`, the rev Crust pins
(Context, "Moho's rev").

- **M1. Cross-input light linking.** *Closed:* `moho::chain::input(stage)` is the
  standard chain without light linking, `moho::chain::merged(merging)` is light linking
  after the merge, judging the lights' complete membership expressions, and
  `chain::standard` is `input` plus linking. Streamed imports are built as
  `chain::input` per input under a `MergingSceneIndex`, with `chain::merged` on top.
  Crust still judges expressions after the last chunk (D9).
- **M2. Prims outside the loaded chunks.** *Closed:* `moho::streaming::unresolved(index)`
  lists the bound materials, and the network nodes outside a material, that no input
  holds. D4's side input is built from it.
- **M3. Authored-ness in the scene globals.** *Closed:* the absolute root carries
  `__usdStageMetadata/<field>/{value, authored}` for `startTimeCode`, `endTimeCode`,
  `timeCodesPerSecond`, `framesPerSecond`, `metersPerUnit`, `upAxis` and
  `renderSettingsPrimPath`. The time range's and `metersPerUnit`'s authored-ness leave
  `stage_facts` (D7).
- **M4. Source paths for prototype prims.** *Closed:* every prim carries Hydra's
  `primOrigin/scenePath`, the USD path, made relative to the prototype root inside a
  USD prototype; the instancing filters keep it. A prototype prim's stage path is the
  instance's prototype joined with it, so warnings and provenance inside prototypes name
  the same prims as the direct importer, and `import-parity` has no exception for them.
- **M5. An index strategy in `moho-spike`.** *Closed:* strategy (D) does (B) through
  Moho; `scripts/streaming-spike.sh --with-d` runs it. Its Moana peak is recorded beside
  (B)'s with the parity runs.
- **M6. Pass collections.** *Closed:* a render pass's `collections` hold
  `renderVisibility`, and `prune`, `matte` and `cameraVisibility` when applied. The
  `prune`/`renderVisibility` warning reads them there.
- **M7. Merged children order.** *Closed (documented):* inputs strongest first, each
  input's children in its own order, then the prims implied by deeper roots. D5 still
  does not rely on it above a chunk root.

## Risks / Trade-offs

- **[Visit order differs, so geometry ids differ, so the image differs]** → D5 walks
  per chunk with the direct importer's stack discipline. The digest compares geometry in id
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
  the ALab and Moana runs measure it, reported, not gated (`import-parity`). Hot arrays (points, indices,
  primvars) are read once as shared `sdf::Value` arrays, never element by element
  through data sources.
- **[Memory]** (the scene input keeps a `LoadNone` stage alive for the whole traversal,
  as the direct importer already keeps its index stage until light links) → it is dropped
  at the same point, and Moana's peak is reported beside the direct importer's and the spike's (B) and (D). Each chunk input is removed and its
  stage dropped before the next opens.
- **[openusd bump friction]** (three repos pinned to one rev) → D1's checks fail loudly.
  The bump order is written in `CLAUDE.md`.
- **[Pending changes touch the same code]** (`payload-and-purpose-view` adds
  `includedPurposes`, binding purposes and load-set flags; `mtlx-format-plugin` moves
  the `.mtlx` arc into composition) → if one lands before this change ends, its
  behaviour lands in the direct readers and the index readers alike, and its baselines
  are recorded as its own behavioural change, not folded into a migration step.
- **[Thread confinement]** (Moho data sources are `!Send`) → the import is already
  single-threaded. `commit_slots` takes arrays, never data sources. Values crossing to
  rayon are `sdf::Value`/`Vec`, which are `Send`.

## Migration Plan

1. Dependencies (D1, D2) land alone. No behaviour change. CI is green with one openusd.
2. The digest and the parity harness land, and the baselines are recorded from the
   direct importer: samples, fixtures, ALab and Moana.
3. The index skeleton replaces the direct traversal, with every family bridged. Parity
   with the baselines holds from here on.
4. One task group per family, each merged only with parity green on the samples and
   fixtures. ALab is run at the materials milestone, ALab and Moana at instancing and at
   the end.
5. The direct readers, the bridge, `EvalTimeScope` and the epoch are deleted. hdCrust's
   scripts, `crust diagnostic`, the MCP tests and `check_images.sh` run against the
   direct importer's recordings. The design record and docs are updated.
6. Rollback is a revert: before step 5, of the family step that broke parity; after it,
   of the change. There is no switch to fall back on.

## Open Questions

- **Primvar inheritance.** USD (and Hydra's flattening) makes an ancestor's constant
  primvar visible to descendants, and Crust reads a prim's own only. Adopting
  inheritance is a behavioural change to `usd-scene-import`, proposed separately after
  this change. It does not change this plan: parity keeps today's behaviour.
- **Light links on `categories`.** M1 has landed. Does Crust read Moho's categories
  instead of judging expressions itself after the last chunk (D9)? An offline import
  removes chunks as it goes, so categories read during the walk would miss lights in
  chunks not yet added; reading them would need a second pass or lights collected
  ahead. Left to the user; the agreement test (single stage and streamed) is there to
  switch on.
