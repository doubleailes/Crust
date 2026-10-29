## Why

Interactivity is the priority: how fast an edit reaches the restarted render matters
more than time to first pixel. `moho-hydra-parity` made Crust a Hydra 2.0 delegate over
Moho and measured its update latency (`openspec/specs/render-delegate/design.md`,
§ Update latency). Moho's share of every edit is microseconds to milliseconds. Crust's
share is what the user waits for:

| edit (median, edit → restarted render) | 1k meshes | 10k meshes | Moana |
|---|---|---|---|
| camera `focalLength` | 1.9 ms | 20 ms | 52 ms |
| camera transform | 1.8 ms | 20 ms | 43 ms |
| add a light, trivial collections | 361 ms | 3.6 s | 817 s |
| add a light, linked to one prim | 418 ms | 3.7 s | 767 s |

A camera edit grows with the scene, though it changes nothing the scene holds. Adding or
removing a light re-commits the whole scene: ids are path-ordered (`moho-hydra-parity`
D3), so a new light shifts the light-list indices and the emitter geometry ids after it.
A points or normals edit on a shared or instanced mesh also re-commits. On Moana, the
process grows from 30.6 GiB to about 44 GiB over a few re-commits.

## What Changes

- **Lights are added and removed in place.** The light sources become their own layer
  of the scene. Every light's emitter geometry is numbered after all other geometry, in
  light path order, and built into its own kernel segment. A light added, removed or
  reshaped rebuilds that layer: the light list, the emitter geometry and its segment.
  The light and shadow links are then judged again over the receivers the commit
  recorded, and installed into the committed world without rebuilding its tree.
  `categories` and `instanceCategories` edits are applied in place the same way. The
  result stays bit-identical to a fresh delegate's.
- **Camera and settings edits do not scale with the scene.** The O(scene) work on the
  settings path goes:
  - the strongest layer's `invisible_overrides` scan on every refresh;
  - the slot → material map that render control rebuilds on every update;
  - the light selection that every restart builds twice.
  The remaining suspects are profiled and named in the design. A test pins that a
  settings update reads no prim outside the settings, products, vars and cameras.
- **Shared and instanced meshes are reshaped in place.** A points, normals or topology
  edit on a mesh that other direct placements share, or that a native-instance or
  point-instancer prototype holds, rebuilds that distinct mesh (and its prototype's
  parts) through the commit's own readers. Every placement then gets the new kernel
  scene. Nothing else is read again.
- **A re-commit releases what it replaces.** The delegate stops holding the old scene
  while it commits the new one. The kept assets are swept on every re-commit, not only
  on the first. The remaining growth is measured by site and either fixed or recorded.
- **The update-latency benchmark grows.** New edits: removing a light, moving an area
  light, reshaping a baked mesh, reshaping a prototype's mesh, a receiver's
  `categories`. New columns: resident and peak memory. A re-commit loop. Each edit has a
  target, reported against it and never gated.
- Images: emitter geometry ids move to the end of the id range, and the emitters get a
  kernel segment of their own. Exact-tie hits between an emitter and other geometry may
  resolve differently than before, once. The goldens are re-checked, and any that move
  are shown to be noise (1/√N), as `moho-hydra-parity` D11 did.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `render-delegate`: the class table applies lights added or removed, `categories` and
  `instanceCategories`, and shared or instanced mesh reshapes in place. A settings
  update reads only the settings, products, vars and cameras. A re-commit holds one
  scene at a time. The update-latency benchmark adds edits, memory and targets. This
  capability's spec is added by `moho-hydra-parity`, which must be archived (PR #422)
  before this change archives.
- `usd-scene-import`: "Light schema mapping": the light list holds the lights in path
  order, and their emitter geometry is numbered after all other geometry and kept apart
  from it in the kernel.
- `intersection-kernel`: a committed scene in segments, one of which can be rebuilt
  alone, and ray masks that can be rewritten in a committed scene. Both give the
  structure a fresh commit gives.

`lighting` keeps its requirements: what a light, a link or a shadow link does to the
image is unchanged.

## Impact

- **crust-rt**: `scene/mod.rs` gets segments (`SceneBuilder`, `Scene::rebuild_segment`)
  and in-place mask rewrites. `bvh/` is traversed per segment.
- **crust-core**:
  - `rt_world.rs`: the light layer's id range, `World::replace_light_layer`, and link
    tables installed on a committed world.
  - `light/list.rs`: the light list rebuilt whole from the layer.
  - `scene/hydra/`:
    - `commit.rs`: lights collected during the walk and emitted after it; the class
      table.
    - `light_links.rs`: judging split from installing, and the record kept.
    - `update.rs`: the light layer, re-linking, shared reshapes.
    - `instancing.rs` and `mesh.rs`: prototype parts rebuilt for a reshape.
    - `delegate.rs`: the old scene dropped before a re-commit.
  - `live.rs` (`Provenance`): the link record, the per-slot readers and the prototype
    placements.
  - `usd_import/stage_facts.rs`: `invisible_overrides` read once.
- **crust-render**:
  - `delegate/mod.rs`: the pick map built lazily, one light selection per restart, and
    the kept assets swept on a re-commit.
  - `examples/update_latency.rs`: the new edits, memory and the re-commit loop.
- **Tests**:
  - `crust-core/tests/live.rs`: a bitwise pair per new in-place case, and a counting
    index for settings reads.
  - `crust-render/tests/delegate.rs`.
  - `crust-rt` tests for segments and mask rewrites.
  - `scripts/check_images.sh` over the goldens.
- **Docs**: `openspec/specs/render-delegate/design.md` (classes, latency, memory),
  `docs/architecture.md` (the kept delegate, the in-place ↔ fresh-commit invariant, the
  light layer) and `openspec/specs/intersection-kernel/design.md`.
- **Throughput**: a ray in a scene with area lights traverses a second, small segment.
  This is measured with `scripts/bench_ab.sh` and callgrind, and a regression is
  reported before the change lands.
- **Moho**: no change is planned. The gaps found are listed in the design (§ Moho gaps)
  and go to Moho.
- No new dependency. No `unsafe`.
