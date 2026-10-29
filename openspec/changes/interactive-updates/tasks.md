## 1. Prerequisites

- [ ] 1.1 Confirm `moho-hydra-parity` is archived (PR #422), so `openspec/specs/render-delegate/spec.md` exists. Rebase this change's `render-delegate` and `usd-scene-import` deltas on the archived text if it moved.
- [ ] 1.2 Get the user's answers to design Q1–Q5. Q1 and Q3 change tasks 3–5. Record the answers under design "Decisions awaiting the user".
- [ ] 1.3 Record the baseline: the 47 goldens (`scripts/check_images.sh record target/goldens/interactive-updates-base`), `scripts/bench_scenes.sh` on the area-lit samples (`cornellbox`, `usdlux`, `many_lights`), and callgrind instruction counts for `cornellbox` at `-s 2`.

## 2. Benchmark and measurement first

- [ ] 2.1 `crates/crust-render/src/delegate/mod.rs`: add `revalidate` to `UpdateSplit` and time `assets.revalidate()` into it, so the split sums to the update.
- [ ] 2.2 `crates/crust-render/examples/update_latency.rs`: print resident (`crust_core::current_memory_bytes`) and peak memory after setup and after each edit.
- [ ] 2.3 `update_latency.rs`: add the edits "remove a light" (timed, replacing the untimed removal), "move an area light", "reshape a baked mesh", "reshape a prototype's mesh" and "include a mesh in a light's `collection:lightLink`".
- [ ] 2.4 `update_latency.rs`: extend the fixture with `--instanced P×M`: a native-instanced prototype mesh placed M times and a point instancer placing it P times.
- [ ] 2.5 `update_latency.rs`: add `--recommit K`, which forces K `full` updates (a mesh defined and removed) and prints resident memory after each.
- [ ] 2.6 Run the extended benchmark on main's behaviour (fixture 1k, 10k and Moana) as the "before" table, with a heaptrack run of `--recommit 3` on the 10k fixture and a `MALLOC_ARENA_MAX=2` run (design D6). Keep the numbers for task 9.

## 3. Settings and camera edits that do not scale (design D4)

- [ ] 3.1 Profile a `focalLength` edit with `perf record` on the 10k fixture and on Moana, and confirm or strike each of S1–S7 in design D4. Add any new suspect to the table.
- [ ] 3.2 S1: read `invisible_overrides` once at the commit into `Provenance::stage` (`crates/crust-core/src/live.rs`, `scene/hydra/mod.rs::commit_scene`), and use it in `scene/hydra/update.rs::refresh_settings` instead of `HostFacts::invisible_overrides()`.
- [ ] 3.3 S2: build `Held`'s slot → material map lazily, on the first `pick` after a commit, cached until the next commit (`crates/crust-render/src/delegate/mod.rs`).
- [ ] 3.4 S3: build the light selection once per restart, with the launch settings. Use `Held::of` + `launch` in `delegate/mod.rs` together with `Renderer::new` / `reconfigure` / `retune` in `crates/crust-core/src/tracer/mod.rs`. Check that `crust-render/tests/delegate.rs` still agrees with `crust render` bit for bit.
- [ ] 3.5 S4–S6: fix what the profile shows. File what lies in Moho as a gap (design § Moho gaps).
- [ ] 3.6 Test (`crates/crust-core/tests/live.rs`): wrap the delegate's chain in a scene index that counts the prims read. A camera move on a 1 000-mesh scene reads no mesh, material or light prim ("A settings update reads only the settings and cameras").

## 4. Kernel segments and mask rewrites (crust-rt)

- [ ] 4.1 `crates/crust-rt/src/scene/mod.rs`: `SceneBuilder::close_segment()`. `commit` / `commit_with` / `commit_ref` build one BVH per segment, with global geometry ids.
- [ ] 4.2 `crates/crust-rt/src/scene/mod.rs`, `bvh/mod.rs`: closest-hit traverses segments in order, each with the running closest as `t_max`; occlusion stops at the first hit; `visit`, `bounds`, the counters and `MemoryFootprint` cover every segment.
- [ ] 4.3 `Scene::rebuild_segment(i, geometries)` rebuilds one segment from new geometries.
- [ ] 4.4 `Scene::set_mask(geom_id, mask)` patches the mask of every primitive of that geometry in the leaves (`bvh/collapse.rs`'s packet lanes, instance and curve records).
- [ ] 4.5 Tests in `crates/crust-rt/src/scene/tests.rs`: a one-segment scene's layout and query results are bit-identical to before; rebuilding segment 1 equals a fresh two-segment commit; a closest hit across segments; equal distances keep segment 0; a rewritten mask equals a fresh commit with that mask over a fixed ray set; two mask sets give identical layouts.
- [ ] 4.6 Run `scripts/test_simd_matrix.sh -p crust-rt` and the pinned nightly `bvh8` leg (CLAUDE.md) over the new paths.

## 5. The light layer (design D1, D3)

- [ ] 5.1 `crates/crust-core/src/scene/hydra/commit.rs`: `commit_prim` records each light in `ctx.light_prims` (path and world transform) instead of calling `emit_walked_light`. The walk's `saw` no longer sees emitters.
- [ ] 5.2 `crates/crust-core/src/scene/hydra/mod.rs`: `emit_light_layer` runs `emit_light` over `ctx.light_prims` in path order after `visit_all` and before `flush_meshes`, behind a `close_segment()`. Move `LightLinks::light_added`, the active pass's `light_added` and `light_origins` there.
- [ ] 5.3 `crates/crust-core/src/rt_world.rs`: `WorldBuilder` closes the segment and `World` records `light_base` (G). Keep the receivers' part of each world flag (`bounce_pass_throughs`, `bounce_side`, transparent emitters, holdouts) apart from the layer's.
- [ ] 5.4 `rt_world.rs`: `World::replace_light_layer` truncates every per-id table and the kept inputs to G, appends the layer's, calls `rebuild_segment(1, …)` and recomputes the flags. `check` refuses an emitter with motion.
- [ ] 5.5 `crates/crust-core/src/light/list.rs`: the light list is replaced whole by the layer's (lights, names, tags, gains, infinite masks, backdrops). Keep `LightList::check_replace` / `replace` for the per-light parameter edit.
- [ ] 5.6 `crates/crust-core/src/scene/hydra/update.rs`: `Rebuilding::light_layer(added, removed)` rebuilds the layer through `emit_light_layer` in a scratch context whose first id is G. Apply the guards of design D3 (active pass, warnings), and rebuild `Provenance::lights` and the emitter runs. Route an area light's `xform` / shape edit through it instead of `GeometryEdit` + `replace_geometry`.
- [ ] 5.7 `crates/crust-core/src/scene/hydra/commit.rs::classify` (design D8): a light added or removed fills `change.layer` instead of `recommit`. `scene/hydra/delegate.rs::apply` applies it in place.
- [ ] 5.8 Goldens: `scripts/check_images.sh check` against 1.3's recording. For every golden that moves, show the beauty's relmse against the baseline falling as 1/√N (16, 64, 256 spp, `--variance-threshold 0 --indirect-clamp 0`). List every output that exposes a geometry id (pick, `crust light-paths` light indices, identity AOVs) and check it. Record the results in `openspec/specs/usd-scene-import/design.md`.
- [ ] 5.9 Throughput: `scripts/bench_ab.sh` (main vs this branch) on `cornellbox`, `usdlux`, `many_lights` and one sample without area lights, and callgrind on `cornellbox`. Report the result against Q3's budget in `openspec/specs/intersection-kernel/design.md`.

## 6. Light links on a committed world (design D2)

- [ ] 6.1 `crates/crust-core/src/scene/hydra/light_links.rs`: split `resolve` into `judge(&record, &LightList) -> LinkTables` (lights to demote, receiver classes, shadow bits, runtime links, volume classes and masks, warnings) and `install(LinkTables, …)` for a `WorldBuilder`. The commit calls both, and no golden moves for the split alone.
- [ ] 6.2 Keep the receivers' link record (paths, categories, runs, volume runs) in `Provenance` (`crates/crust-core/src/live.rs`) instead of consuming it.
- [ ] 6.3 `rt_world.rs`: `World::install_links(LinkTables)` rewrites `light_classes`, patches only the masks that change through `crust_rt::Scene::set_mask` (and in the kept inputs), and sets the volumes' classes and masks. Demotions act on the light layer.
- [ ] 6.4 The light layer rebuild (5.6) judges and installs the links. The fast path: no light restricted before or after means the tables are extended, not judged.
- [ ] 6.5 `classify`: `categories` / `instanceCategories` fill `change.categories`. The update re-reads those receivers' categories with `receiver_categories`, updates the record and judges again.
- [ ] 6.6 `crates/crust-core/tests/live.rs`, a bitwise pair against a fresh delegate for each: a light with trivial collections added; a light linked to one prim added; a shadow-linked light added among shadow-linked lights (the bit ranking moves); an area light removed before another in path order; a dome removed; a receiver included in a `lightLink` (categories dirtied); a light linked to nothing added (demoted). Each also asserts the update was applied in place (the scene stats record "Update in place (lights)").

## 7. Shared and instanced meshes reshaped in place (design D5)

- [ ] 7.1 `crates/crust-core/src/live.rs`: `Provenance` records each mesh slot's readers (direct `geom_id`s, prototype key and part) and, per prototype key, its parts' layout and the top-level `geom_id`s of each part's placements. Fill them in `scene/hydra/mesh.rs::flush_meshes` and `scene/hydra/instancing.rs` (`emit_native_instance`, `emit_point_instancer`, `attach_proto_parts`).
- [ ] 7.2 `scene/hydra/mesh.rs`: factor `flush_meshes`'s bake-or-instance decision into one function that the commit and the rebuild both call.
- [ ] 7.3 `scene/hydra/update.rs`: case 2, a direct mesh shared with other direct placements. Split the slot, decide both slots again, and emit a `GeometryEdit` per affected placement (instance ↔ baked included).
- [ ] 7.4 `scene/hydra/update.rs` + `instancing.rs`: case 3, a prototype's mesh. Rebuild its parts with `prototype_parts` / `build_parts` over fresh caches. Refuse a changed layout, and emit a `GeometryEdit` per placement of each part (new `Arc<RtScene>` and side tables).
- [ ] 7.5 `live.rs::reshapable` and `commit::classify`: send the shared and prototype cases to `change.reshaped` under design D5's refusals (adaptive, displaced, moving, nested instancer, a slot read by both direct placements and a prototype or by two prototypes, a new key equal to an existing slot's).
- [ ] 7.6 Check that a points edit on a prototype's prim reaches the delegate on the Hydra prototype path for a native instance and for a point instancer. If it does not, record Moho gap G3 and keep the case refused.
- [ ] 7.7 `crates/crust-core/tests/live.rs`, bitwise pairs: one of two identical direct meshes reshaped; a native-instance prototype's mesh reshaped; a point-instancer prototype's mesh reshaped; a reshape that makes two meshes identical (reports `full`).

## 8. Memory over re-commits (design D6)

- [ ] 8.1 `crates/crust-core/src/scene/hydra/delegate.rs::apply`: decide the class before committing, and drop the old `Scene` before a re-commit reads the index. A failed commit leaves the delegate without a scene and keeps the notices pending.
- [ ] 8.2 `crates/crust-render/src/delegate/mod.rs::update`: sweep the kept assets after every commit from scratch, not only when there was no scene.
- [ ] 8.3 Test ("A re-commit holds one scene"): a weak reference to the old world is dead when the new commit reads its first prim (`crates/crust-core/tests/live.rs`, through an index wrapper that checks the weak on first read).
- [ ] 8.4 Re-run 2.6's `--recommit 3`, heaptrack and `MALLOC_ARENA_MAX=2` runs. Fix the remaining attributable growth in Crust. Record allocator retention (Q4) or Moho caches (G4) as found.
- [ ] 8.5 If 8.4 attributes growth to allocator retention (M3), adopt mimalloc or jemalloc as the `crust` binary's global allocator (user decision Q4), choosing by the `--recommit 3` memory and `bench_scenes.sh` throughput of both. Add it to `deny.toml`'s review, note the allocator's `unsafe` in CLAUDE.md's `unsafe` rules and the PR checklist, and keep every Crust crate `#![forbid(unsafe_code)]`. Record the measurements in the design record.

## 9. Benchmark, record and docs

- [ ] 9.1 Run the benchmark (fixture 1k and 10k at 50 runs, `--instanced`, `--recommit 3`, Moana at 10 runs under `systemd-run --scope -p MemoryMax=80G`) at this change's head, alone on the machine.
- [ ] 9.2 `openspec/specs/render-delegate/design.md`:
  - update § Classes, the table and the "re-committed" paragraph;
  - add the light layer and the link install to § Rebuilds;
  - put the new tables in § Update latency, against design D7's targets and the before table of 2.6;
  - add a § Memory over re-commits;
  - add the Moho gaps found.
- [ ] 9.3 `docs/architecture.md`:
  - the kept-delegate diagram (lights in place, re-commit drops the old scene);
  - § Invariants: the light layer ↔ its emission in a fresh commit, judge ↔ install on a builder and on a world, masks never read by the BVH build;
  - the technical-debt register (a geometry edit's top-level rebuild, Q1-R′).
- [ ] 9.4 `openspec/specs/intersection-kernel/design.md`: segments and mask rewrites, and the throughput measurement of 5.9.
- [ ] 9.5 `site/content/docs/architecture/` pages that describe live updates or geometry ids: update them if any states that adding a light re-imports the scene. Build with `zola build` 0.21.
- [ ] 9.6 The three checks (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), `cargo clippy -p crust-render --features delegate --all-targets -- -D warnings`, `cargo test -p crust-render --features delegate --test delegate`, and `openspec validate interactive-updates --strict`.
