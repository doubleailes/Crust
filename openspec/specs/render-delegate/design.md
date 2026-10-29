# render-delegate — design record

> Design record for the **render-delegate** capability: Crust as a Hydra 2.0 render
> delegate, handed a scene index by its host and updated in place from its notices, and
> the render control a host polls. The reasoning, measurements and history behind the
> behaviour `spec.md` states. `openspec/specs/*/design.md` is the whole record;
> `docs/architecture.md` is the map. The capability was founded by the change
> `moho-hydra-parity` (its design D1–D14). Two parts come from the retired
> `hydra-delegate` record of hdCrust, the C++ Hydra plugin that change removed: how a
> Hydra host hands a delegate its scene index, and how a panic is contained.

## How a Hydra host hands a delegate its scene index

Read from OpenUSD 26.08's headers and the v26.08 and v26.11.1 sources for hdCrust
(`hydra-render-delegate` D1), and still the shape a later Hydra host sits on:

- **26.x has no usable native 2.0 renderer interface.** `HdRenderer`
  (`pxr/imaging/hd/renderer.h`) is a stub ("TODO: Add API here"). Its only surface is
  `GetLegacyRenderControl()`, which returns the "transitory"
  `HdLegacyRenderControlInterface`. A plugin overriding `HdRendererPlugin::_CreateRenderer`
  would have to reimplement usdview's whole Hdx task graph; no renderer in Pixar's tree
  does, in 26.08 or 26.11.
- **`HdRenderDelegate` is the entry point, and it already sits on the scene index.**
  usdview's engine (`usdImagingGL/engine.cpp`, scene index mode, the default) calls
  `plugin->CreateRenderer(terminalSceneIndex, args)`. For a plugin without
  `_CreateRenderer`, the base class builds an `HdRenderDelegateAdapterRenderer`, which
  creates the delegate and a render index through `HdRenderIndex::NewForBackendEmulation`.
  The render index then:
  - calls `HdRenderDelegate::SetTerminalSceneIndex(terminalSceneIndex)` ("Hydra 2.0
    API") once the scene index graph exists: where a delegate observes it;
  - calls `HdRenderDelegate::Update()` at the start of every `SyncAll`, before any prim
    sync: where a delegate applies what it collected.
- **Rprim and Sprim `Sync` calls do nothing.** The `HdRprim::Sync` path is deprecated on
  26, gives per-prim callbacks with no locators, and would be rewritten within a release
  cycle.
- **Supported prim types filter the render index, not the scene index.** The observer
  sees the terminal scene index whole; only the emulation consults
  `GetSupported{R,S,B}primTypes`. A delegate that reads its prims from the scene index
  declares no rprims, the sprim `camera` (the render pass state's `HdCamera` needs it) and
  the bprim `renderBuffer` (AOV bindings).

hdPrman's Riley path (`renderDelegate.cpp`, `rileyPrimFactory.cpp`) is the model Crust's
delegate follows over Moho: the terminal index wrapped in
`HdsiPrimTypeNoticeBatchingSceneIndex`, observed with `HdsiPrimManagingSceneIndexObserver`
and a prim factory, flushed in `Update`. Moho ports the three
(`PrimTypeNoticeBatchingSceneIndex`, `PrimManagingObserver`, `PrimFactory`), so the
delegate's code is the same over Moho as over a later FFI `SceneIndex`.

## Panics are contained where the host unwinds

hdCrust built under an unwinding profile and caught a panic at every boundary
(`hydra-render-delegate` D8). What survives:

- **The CLI's `release` profile aborts.** Nothing in the renderer depends on unwinding,
  and dropping it removes the landing pads around the `expect()`s and the indexing in
  the bounce loop (`Cargo.toml`). `crust render`'s goldens and instruction counts are
  those of `abort`.
- **A host that wants containment sets `panic = "unwind"` in its own workspace**
  (Mantle). Render control then runs each entry point's body in `catch_unwind` and
  returns a panic as an error; the render thread is joined, and a join `Err` is the same
  error. After a panic the delegate drops its committed scene, and the next update
  commits afresh.
- **Report a panic once.** A host that polls and raises an error per failed poll
  printed hdCrust's panic on every redraw, forever: the first poll after a panic returns
  it, later ones do not, and the stopped render counts as converged.
- **`catch_unwind` lives in render control only** (`Cargo.toml`'s rule).

## Render control

`crust_render::Delegate` (feature `delegate`, `crates/crust-render/src/delegate/`) is
what is left of the `crust mcp` session and `LiveSession` once the layer plumbing went
(`moho-hydra-parity` D9, D14). It is the analogue of an `HdRenderPass` with its
`HdRenderBuffer`s, around `crust_core::SceneDelegate`, the scene side.

- **The host owns the stage.** It opens the stage, keeps its `StageSceneIndex` and Moho's
  standard chain, edits the stage, sets the time, applies the index's pending updates,
  and calls `Delegate::update(&facts)` from its own thread, the one that owns the index
  (Moho's indices are `Rc`). The delegate opens no file and follows no layer.
- **An update** cancels and joins the running render, brings the scene up to date
  (`SceneDelegate::update`, below), and restarts the render when the scene changed. An
  `unchanged` update keeps the running render.
- **Polls go through a `RenderHandle`**, which any thread may hold: `snapshot`, `aov`,
  `converged`, the outcome, `cancel_render`, which is one atomic store and never waits
  behind an update, and `pick`, which names the prim and material under a pixel from
  the scene's provenance (`name_hit`). The handle shares the committed scene with the
  delegate as an `Arc<Held>` behind a lock, published when a render starts. An update
  takes it away under that lock first thing (a pick in flight, one ray, finishes
  first), so a pick while it runs is `Busy` rather than a hit in the scene it is
  replacing (`a_pick_during_an_update_is_busy`); an `unchanged` update puts it back as
  it returns. A changing one drops it before it takes the scene back with
  `Arc::try_unwrap`: a handle that still held it would make every update commit
  afresh, which `a_pick_from_another_thread_leaves_updates_in_place` guards. A start
  that fails (a region outside the frame, a thread that will not spawn) changes nothing
  the host polls: the prior render stays current, and its picks answer
  (`a_failed_start_keeps_the_prior_renders_picks`). Dropping the delegate takes the
  copy away too, so a handle the host keeps holds the last image but not the scene. A host whose delegate stays on
  the index's thread (Mantle's IPR) picks from its viewer's thread this way.
- **Where an update's time goes** is `Delegate::last_split()`: `stop` (cancelling the
  render), `flush` (Moho's notice batching), `commit` (classifying and rebuilding or
  re-committing), `restart` (`Renderer::new` and the next render's start).
- **Hooks** (`on_thread`, `on_warnings`) and panic containment are `live-session-host`
  D1's (§ Panics above).

The CLI is the batch host and does not use `Delegate`: it updates a `SceneDelegate` once
and renders (`load_scene`). `crates/crust-render/tests/delegate.rs` drives a `Delegate`
as a Hydra host does, and pins that its final image is `crust render`'s bit for bit.

## Kept assets

The delegate owns one `FileAssets` built with `keep_decoded()` and passes it to every
commit and rebuild. The loader keeps every texture, Ptex, light texture and IES profile it
decoded (`crust-assets/src/kept.rs`), stamped with the length and modification time of
each file the decode read. That includes, for a UV texture, each UDIM tile and the
`.tx` sibling the streaming path looks for, and for a `<UDIM>` / `<UVTILE>` set its
directory.

- **Revalidation.** Each update first calls `revalidate()`, which drops the entries
  whose stamps moved. A file changed on disk sends the index no notice, and the scene's
  materials and lights still hold the old decode, so any dropped entry makes the update
  commit afresh, whatever the batch (`a_texture_changed_on_disk_is_decoded_again`).
- **Sweeping.** Before a commit from scratch, `sweep()` lets go of what the last commit
  did not ask for.
- **Only successes are kept.** A file that failed is tried again by the next commit,
  which raises the warnings a fresh loader would.
- **Environment maps are not kept.** They cross `AssetLoader` by value.
- **JIT programs belong to the materials.** A re-commit recompiles every MaterialX
  program, and an in-place material update compiles only the changed material.
- **Switch.** `CRUST_SCENE_DELTAS=0` gives a fresh loader per commit and makes every
  update `full`.
- *Accepted:* a file rewritten without its length or modification time changing is not
  seen.

## Updates in place

### Where an update's time goes

The BVH commit is a small share of an import: 19.1 s of Moana's 5:13 parse and 1.4 s of
ALab's 2:57 (`docs/moana_profile.md`, `docs/alab_profile.md`), measured with the
direct importer. Composition, the traversal and, on textured stages, asset decoding
dominate. Keeping the traversal and refitting the BVH would save little, and a refit
tree is not the tree a fresh build gives. The win is in not committing at all and in not
decoding again.

### Classes from the batch's notices (D9)

`commit::classify` decides from the batch alone: the managed prims added and removed by
kind, and the locators dirtied. It does no stage diffing and does not re-read the index
to classify. Moho's generic locators (`__usdAttributes`, `__usdRelationships`) are not
read, so a change there classifies as nothing.

| batch | class | applied |
|---|---|---|
| empty, or only generic locators, or an untyped prim's `xform` / `visibility` / `purpose` / `primvars` (they reach the prims below as their own flattened entries) | `unchanged` | the render keeps running |
| a render settings, product or var prim dirtied; the render camera's `camera` or `xform`; the root's `sceneGlobals` (a new current frame, another active settings prim) | `settings` | in place; the guards below |
| a used material's `material` | `materials` | in place |
| a light's `light` or `material` (an infinite light's `xform` too) | `lights` | in place: the light alone, or the light layer when its link ids moved or the links took it out of the list |
| a light added or removed; a light's `collections`; a receiver's `categories` or an instancer's `instanceCategories` | `lights` | in place: the light layer |
| an area light's `xform` | `geometry` | in place: the light layer |
| a direct mesh's `xform` alone (a move); a mesh's `mesh` or `primvars`, with or without its `xform` (a reshape, read at its transform now): placed once and baked, or one placement of a mesh other direct placements share (the slot splits); a sphere's `xform` | `geometry` | in place (`GeometryEdit`) |
| a mesh of a native-instance or point-instancer prototype (`mesh`, `primvars`) | `geometry` | in place: the prototype's parts, at every placement |
| anything else; two distinct in-place classes in one batch | `full` | committed afresh |

**Committed afresh** happens for `full`, and for an in-place class a guard refuses (an
active render pass under a light layer rebuild, a warning the scene carries about the
rebuilt prims or one the rebuild raises, a reshape a fresh commit would share), which
then reports `full`. The old scene is dropped first (`SceneDelegate::commit_afresh`),
and the kept assets are swept after (`interactive-updates` D6).

### Provenance

`Scene::provenance` (`crust_core::live::Provenance`) records what the commit already
builds:

- material prim path → the `World` slots holding its `Arc`, and whether a mesh was
  displaced by it;
- light prim path → its index in the `LightList` after light-link resolution, and the
  `geom_id`s it emitted.
  - *Trap:* resolution takes a light linked to no receiver out of the list and shifts
    the later ones down. `LightLinks::resolve` returns its remap (`index_remap`), and a
    light taken out gets no entry.
- the geometry runs `LightLinks` interns (`Provenance::prim_of_geometry`);
- what a move or a reshape needs (`movable`, `reshapable`), and each baked mesh's
  content key;
- the stage-wide facts a settings change must not move:
  - the host's options and the `RenderSettings` path;
  - the subdivision level and what tessellation read from the camera (the `ScreenKey`);
  - the dome's camera visibility and the working space;
  - the render camera, its shutter, and the first camera.

Memory is in proportion to the material, light and emitting-prim counts, nothing per
triangle.

### Rebuilds go through the commit's own code

`hydra/update.rs::rebuild` reads the delegate's index and the host's facts, at the
index's time, with the kept loader. It never copies a commit function, since
bit-identity would then rest on two copies agreeing.

- **Materials** go through `resolve_fresh`, what a material cache miss runs, and
  `resolve_displacement`. A material that defines a displacement is refused, since its
  inputs shape geometry.
- **Lights** go through `emit_light`, the commit's dispatch, into a scratch context
  whose `WorldBuilder` has the light's first `geom_id`s claimed first. A rebuild is
  refused when it would not emit exactly one light over the same geometry ids. Light
  links do not refuse it: everything resolution produced is keyed by index or
  `geom_id` and kept.
- **Settings** go through `read_index`, the commit's function, and then the camera and
  `apply_exposure`. They are refused when any of these would change:
  - the working space, the subdivision level or the dome's camera visibility;
  - the shutter;
  - the products' kept primvars or the precomp;
  - the `ScreenKey` that tessellation was handed: an exposure stays in place, while a
    move of a dicing camera, a lens or a resolution re-commits.
- **Geometry** is a `GeometryEdit` per moved or reshaped mesh, from the same mesh
  readers, after which the world commits the receivers' tree again (the light
  layer's segment is untouched). A mesh another direct placement shares takes a
  distinct mesh of its own and bakes, and so does the one left alone
  (`flush_meshes`'s decision, `mesh::bakes`). A prototype's mesh rebuilds its parts
  with the commit's prototype walk over fresh caches, and each placement gets the new
  kernel scenes at the transform it had (`Provenance::prototypes`).
- **The light layer** (`interactive-updates` D1–D3): the lights the provenance
  holds (`light_paths`, demoted ones included), less those the batch removed and
  plus those it added (`commit::walked_light` decides a new path as the walk would),
  emitted in path order by the commit's `emit_light_layer` into a world starting at
  `World::light_base`; the links judged (`judge`) from the receivers' record in the
  provenance (their categories read again when dirtied) and the layer's, and
  installed (`install`) on the world after `World::replace_light_layer` swapped the
  layer and rebuilt its kernel segment alone. Masks the links rewrote are patched in
  the leaves (`crust_rt::Scene::set_masks`); a world keeps the receivers' unlinked
  masks to give them back. Refused under an active render pass, when the scene
  carries a light, IES or link warning, or a warning names a light, and when the
  rebuild raises any warning.
- **Warnings.** A rebuild is refused whenever the scene's warnings might not be what a
  fresh commit reports:
  - a material or light rebuild, when the warnings name the prim, or when the rebuild
    raises a warning the scene does not carry;
  - a settings refresh, when the scene carries a warning of a family a setting can
    move, or the refresh raises any.

`Update::check` then `Update::apply`:
- `World::check_materials` compares each rebuilt material's `Signature` with the one in
  every slot it goes into;
- `LightList::check_replace` refuses another light kind, a move to or from infinity, or
  another `geom_id`.

The renderer is rebuilt with `Renderer::new`, never `retune`, so the light selection
is a fresh one's. Any refusal re-commits, and the update reports `full`.

### In place ↔ a fresh commit is a bit-identity pair

`crates/crust-core/tests/live.rs` drives a host over a stage scene index. For each
in-place class it edits the stage, updates, and compares the scene, rendered at 16
spp, with a fresh delegate's commit of the same index, every plane bitwise. The edits
are:
- a shader input;
- a light's intensity, unlinked and linked, and a light added;
- a mesh's transform;
- an exposure;
- a time change that moves the camera.

The same file pins the re-commits: adaptive focal length, a shutter, the settings
guards, a precomp, kept primvars, a prim added. `commit.rs`' tests pin that two notice
orders commit to bitwise-equal scenes.

### History: the `crust mcp` session

The classes began as an allow-list over a layer diff in the `crust mcp` session
(`live-stage-scene-deltas`), applied through `Provenance::rebuild` over masked stages.
The session and its traps went with `moho-hydra-parity`. Its measured edit latency, min /
mean of 10 edits each, release build, 4 cores, `CRUST_SCENE_DELTAS` on and off:

| stage | open | `settings` | `lights` | `materials` | `full` |
|---|---|---|---|---|---|
| synthetic: 16 quads, each its own 2048² PNG, on | 0.81 s | 1.7 / 1.9 ms | 0.9 / 1.0 ms | 1.2 / 1.3 ms | 8.9 / 9.1 ms |
| the same, off | 0.76 s | 608 / 632 ms | 633 / 662 ms | 634 / 659 ms | 624 / 644 ms |
| `usdpreview_textured`, on | 18 ms | 1.0 / 1.2 ms | 0.6 / 0.7 ms | 0.9 / 1.3 ms | 4.3 / 5.1 ms |
| the same, off | 15 ms | 5.7 / 6.5 ms | 5.8 / 6.3 ms | 5.5 / 6.5 ms | 5.6 / 6.1 ms |

## Update latency

Design D12: how long an edit takes from the stage to Crust's restarted render. Reported,
not gated; update latency ranks above first-load time.

### Method

`crates/crust-render/examples/update_latency.rs` (feature `delegate`) is a host:
- it opens the stage with payloads loaded and keeps its `StageSceneIndex`;
- it builds Moho's standard chain with the scene globals filter;
- it keeps a `Delegate` rendering at 64×36 (the example authors its own
  `RenderSettings`).

One run:
1. authors the edit on the stage (openusd);
2. calls `apply_pending_updates`;
3. calls `Delegate::update`, which cancels and joins the render, flushes the batched
   notices, brings the scene up to date and restarts the render;
4. stops the clock when the render id has moved.

The four edits and the fixture are Moho's (`docs/update-latency.md` in Moho):
- a camera's `focalLength`;
- its first transform op;
- a sphere light with trivial collections added;
- one whose `lightLink` includes one prim, added.

An added light is removed after its run, untimed. Each table gives the median (the mean
of the middle two for an even N) and the worst run. Then comes the class and the split of
the median run: authoring, `apply_pending_updates`, and the delegate's `stop`, `flush`,
`commit` and `restart` (`Delegate::last_split`), in ms.

```sh
cargo build --release -p crust-render --features delegate --example update_latency
target/release/examples/update_latency --runs 50 --fixture 10000
systemd-run --user --scope -p MemoryMax=80G -p MemorySwapMax=0 \
    target/release/examples/update_latency --runs 2 --camera /island/cam/shotCam \
    --linked /island/isCoral/isCoral5/geometry/hires_part/starhornstem0004_geo \
    ~/Workspace/samples/island/usd/island.usda
```

Machine: Intel Xeon E5-2699 v4 (2×22 cores, 72 threads), 93 GiB, Rocky Linux 9 (kernel
5.14), rustc 1.99.0, release profile. crust-render `86ff6033`, Moho `b33cee1`, openusd
`doubleailes/openusd` `d7d2a29`. Run on 2026-10-29, alone on the machine.

### Fixture, 1 000 meshes (50 runs)

| edit | class | median (ms) | worst (ms) | authoring | apply_pending_updates | stop | flush | commit | restart |
|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 1.866 | 2.850 | 0.025 | 0.032 | 0.135 | 0.006 | 1.582 | 0.056 |
| camera transform | settings | 1.789 | 2.376 | 0.030 | 0.029 | 0.129 | 0.006 | 1.535 | 0.051 |
| light, trivial collections | lights | 360.682 | 442.938 | 0.154 | 0.463 | 0.337 | 0.008 | 359.601 | 0.109 |
| light linked to one prim | lights | 417.643 | 451.111 | 0.483 | 5.082 | 0.002 | 0.013 | 411.175 | 0.135 |

### Fixture, 10 000 meshes (50 runs)

| edit | class | median (ms) | worst (ms) | authoring | apply_pending_updates | stop | flush | commit | restart |
|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 20.332 | 21.723 | 0.025 | 0.034 | 0.127 | 0.006 | 20.084 | 0.063 |
| camera transform | settings | 19.831 | 27.819 | 0.041 | 0.031 | 0.101 | 0.006 | 19.577 | 0.059 |
| light, trivial collections | lights | 3647.325 | 4498.018 | 2.169 | 1.626 | 0.002 | 0.007 | 3643.367 | 0.125 |
| light linked to one prim | lights | 3745.421 | 4359.669 | 3.491 | 48.550 | 0.002 | 0.014 | 3693.615 | 0.101 |

### Moana island

`island/usd/island.usda`, `LoadAll`, camera `/island/cam/shotCam`, the linked light
including `/island/isCoral/isCoral5/geometry/hires_part/starhornstem0004_geo`, under
`systemd-run --scope -p MemoryMax=80G -p MemorySwapMax=0`.

- **Runs.** 2 runs instead of the planned 10: a light edit re-commits the island in
  about 13 minutes, and so does its untimed removal, so 10 runs of the two light edits
  would take about 9 hours.
- **Setup** (stage, chain, delegate, first commit and render start): 956 s, peak 30.64
  GiB.
- **Memory.** The process grew to about 44 GiB over the re-commits: what a re-commit
  replaces is not all released. It is a follow-up with the light insertion below.

| edit | class | median (ms) | worst (ms) | authoring | apply_pending_updates | stop | flush | commit | restart |
|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 51.529 | 60.893 | 0.385 | 0.334 | 3.089 | 0.023 | 28.012 | 19.685 |
| camera transform | settings | 42.627 | 43.341 | 0.082 | 0.100 | 0.226 | 0.014 | 24.148 | 18.056 |
| light, trivial collections | lights | 816667.149 | 851851.064 | 177.609 | 251.975 | 5.733 | 0.047 | 816103.913 | 127.860 |
| light linked to one prim | lights | 767396.176 | 775539.457 | 402.597 | 3577.892 | 0.004 | 0.070 | 763277.826 | 137.776 |

Moho measures 0.118 / 0.298 ms for the camera edits on Moana and 101 ms / 2.94 s for
the lights. A camera edit reaches the restarted render in 43 to 52 ms: 24 to 28 ms of
commit and 18 to 20 ms of restart (`Renderer::new` and the render's start; not profiled
further). A light added takes 13 to 14 minutes, all of it the re-commit.

### Reading (before `interactive-updates`)

- **A camera edit is `settings`, applied in place.** Moho's share (authoring,
  `apply_pending_updates`, `flush`) is tens of microseconds, Moho's own figures: 0.023
  and 0.036 ms on 10 000 meshes. The delegate's `commit` grows with the scene: 1.6 ms on
  1 000 meshes and 20 ms on 10 000. The settings refresh re-reads the settings and the
  camera, and something in it walks the scene. That walk is the first profiling target.
- **A light added is `lights`, re-committed** (D9): 0.36 s on 1 000 meshes, 3.6 s on
  10 000 and 13 to 14 minutes on Moana, almost all of it `commit`. Moho's share is what Moho measures (0.66 ms and 46 ms
  for the linked light on 10 000 meshes). True in-place light insertion is the follow-up
  these numbers size: insert into the light list and the world, then re-resolve the link
  masks.
- **`stop` and `restart`** are about 0.1 ms each: cancelling a render and starting the
  next one are not where an update's time goes.

## Update latency after `interactive-updates`

The same method, extended (`interactive-updates` D7): the benchmark authors a rect
light, `/UpdateBenchLights/area`, beside its settings; the fixture adds `/world/baked`
(a quad placed once) and, with `--instanced P×M`, a prototype mesh `/lib/shape` placed
by M native instances and a point instancer placing its own prototype mesh P times.
New rows: a light removed (timed), the area light moved, a baked mesh, a native and a
point-instancer prototype's mesh reshaped, a mesh included in a light's `lightLink`
(the light defined before the runs). New columns: `revalidate` (the kept assets' stat
pass, now in the split) and the resident memory after the edit. `--recommit K` runs K
`full` updates instead and prints the resident memory and the peak during each.

Same machine and protocol, run on 2026-10-29, crust-render `2f7c10a3`, Moho
`c808025`, alone on the machine. *Crust's share* is stop + commit + restart.

### Fixture, 1 000 meshes, `--instanced 100x100` (50 runs)

| edit | class | median before | median after | Crust's share after | authoring | apply_pending_updates | revalidate | stop | flush | commit | restart |
|---|---|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 1.988 | 0.662 | 0.578 | 0.022 | 0.031 | 0.000 | 0.132 | 0.005 | 0.396 | 0.050 |
| camera transform | settings | 1.966 | 0.649 | 0.579 | 0.030 | 0.032 | 0.000 | 0.119 | 0.005 | 0.409 | 0.051 |
| light, trivial collections | lights | 392.611 | 1.765 | 1.412 | 0.044 | 0.274 | 0.001 | 0.326 | 0.004 | 1.002 | 0.084 |
| light removed | lights | 394.184 | 2.665 | 0.917 | 1.597 | 0.133 | 0.001 | 0.245 | 0.005 | 0.616 | 0.056 |
| light linked to one prim | lights | 396.036 | 7.243 | 1.108 | 0.232 | 5.923 | 0.001 | 0.001 | 0.008 | 1.042 | 0.065 |
| area light moved | geometry | 1.489 | 0.813 | 0.703 | 0.033 | 0.038 | 0.001 | 0.115 | 0.005 | 0.533 | 0.055 |
| baked mesh reshaped | geometry | 1.372 | 1.280 | 1.179 | 0.027 | 0.042 | 0.000 | 0.118 | 0.005 | 1.015 | 0.046 |
| native prototype's mesh | geometry (was full) | 383.373 | 2.391 | 1.660 | 0.306 | 0.376 | 0.001 | 0.301 | 0.010 | 1.307 | 0.052 |
| point-instancer prototype's mesh | geometry (was full) | 380.364 | 2.131 | 1.989 | 0.027 | 0.069 | 0.000 | 0.233 | 0.008 | 1.706 | 0.050 |
| mesh included in a lightLink | lights (was full) | 390.850 | 3.294 | 1.445 | 0.031 | 1.722 | 0.001 | 0.265 | 0.011 | 1.123 | 0.057 |

### Fixture, 10 000 meshes, `--instanced 1000x1000` (50 runs)

| edit | class | median before | median after | Crust's share after | target | authoring | apply_pending_updates | stop | commit | restart |
|---|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 21.035 | 0.654 | 0.561 | ≤ 1 ms, ≤ 1.5× 1k | 0.022 | 0.031 | 0.118 | 0.394 | 0.049 |
| camera transform | settings | 20.293 | 0.670 | 0.596 | ≤ 1 ms, ≤ 1.5× 1k | 0.031 | 0.034 | 0.129 | 0.414 | 0.053 |
| light, trivial collections | lights | 4157.842 | 2.314 | 1.457 | ≤ 5 ms | 0.053 | 0.762 | 0.330 | 1.047 | 0.080 |
| light removed | lights | 4182.097 | 23.771 | 1.914 | ≤ 5 ms | 20.836 | 1.002 | 1.135 | 0.701 | 0.078 |
| light linked to one prim | lights | 4330.991 | 51.461 | 1.598 | ≤ 20 ms | 1.201 | 48.438 | 0.001 | 1.529 | 0.068 |
| area light moved | geometry | 7.639 | 0.815 | 0.708 | ≤ 5 ms | 0.033 | 0.039 | 0.122 | 0.533 | 0.053 |
| baked mesh reshaped | geometry | 8.555 | 6.779 | 6.582 | reported | 0.033 | 0.056 | 0.255 | 6.271 | 0.056 |
| native prototype's mesh | geometry (was full) | 4280.665 | 16.453 | 7.714 | reported | 5.043 | 3.609 | 0.314 | 7.340 | 0.060 |
| point-instancer prototype's mesh | geometry (was full) | 4363.456 | 7.537 | 7.392 | reported | 0.033 | 0.080 | 0.365 | 6.973 | 0.054 |
| mesh included in a lightLink | lights (was full) | 4315.179 | 18.243 | 2.057 | — | 0.043 | 16.060 | 0.351 | 1.647 | 0.059 |

Every target is met. A camera edit's Crust share is the same on 1 000 and 10 000
meshes (the strongest layer's walk on every refresh was the term that grew,
`interactive-updates` D4 S1). A light removed waits 20.8 ms on openusd's
`remove_prim` (authoring); a linked light and a lightLink edit wait on Moho's
`apply_pending_updates`. A mesh reshape is bounded by the receivers' top-level tree
rebuild (the debt register, item 6).

### Moana island (10 runs)

Same command and scope as above, `--edits focal,move,light,linked,area,categories`
(the island names no mesh for the reshapes). Setup 1 092 s, resident 30.60 GiB, peak
30.87 GiB.

| edit | class | median before | median after | Crust's share after | target | authoring | apply_pending_updates | revalidate | stop | commit | restart |
|---|---|---|---|---|---|---|---|---|---|---|---|
| camera focalLength | settings | 51.529 | 11.118 | 2.528 | ≤ 5 ms | 0.049 | 0.106 | 8.351 | 1.874 | 0.561 | 0.093 |
| camera transform | settings | 42.627 | 10.603 | 1.859 | ≤ 5 ms | 0.046 | 0.112 | 8.610 | 1.207 | 0.560 | 0.092 |
| light, trivial collections | lights | 816 667 | 221.467 | 119.8 | ≤ 50 ms | 0.294 | 90.851 | 11.702 | 0.699 | 118.946 | 0.160 |
| light removed | lights | (untimed, ~13 min) | 230.430 | 120.8 | ≤ 50 ms | 0.332 | 95.933 | 12.045 | 0.691 | 120.018 | 0.134 |
| light linked to one prim | lights | 767 396 | 3520.857 | 133.6 | ≤ 1 s | 225.934 | 3162.471 | 9.508 | 0.005 | 133.431 | 0.144 |
| area light moved | geometry | — | 126.811 | 114.5 | ≤ 50 ms | 0.086 | 0.158 | 12.097 | 0.537 | 113.863 | 0.117 |
| mesh included in a lightLink | lights | — (full, ~13 min) | 164.269 | 133.0 | — | 0.096 | 21.226 | 10.293 | 0.573 | 131.718 | 0.713 |

- **Camera edits** meet their target: the Crust share is 1.9–2.5 ms (it was 47 ms,
  half of it `Renderer::new`'s light selection built twice and the per-update pick
  map). What is left of the edit is the kept assets' `revalidate`, one `stat` per kept
  file, 8–9 ms on the island (accepted, `interactive-updates` Q5).
- **Light edits** are in place, 3 700 times faster than the re-commit they replace,
  but the trivial light and the area light move miss their 50 ms target by about
  2.4×: the layer rebuild judges the links over every receiver (the island keeps a
  restricted light, `sky_dome_cam_llc`, so the fast path does not apply) and writes a
  class per geometry. Two Moana-only costs were found and removed on the way: the
  dome's environment maps were decoded and importance-sampled again at every layer
  rebuild (16.7 s of an 18.4 s light add; a kept delegate now shares them,
  `lights::Environments`), and every in-place update recomputed the scene counters'
  unique primitives and footprint, which walk every instance (1.7 s). A classifier
  that keeps the receivers' classes while the linked lights' categories do not move
  is the next step.
- **A linked light** costs 3.16 s of Moho's `apply_pending_updates` (Moho gap G1);
  Crust's share is 134 ms, within its 1 s target.

### Memory over re-commits

`--recommit 3`, resident memory after each re-commit and the peak during it (GiB):

| scene | after setup | after 1 | after 2 | after 3 | 3 / 1 | peak during 1 / 2 / 3 |
|---|---|---|---|---|---|---|
| fixture 10k, before | 0.103 | 0.112 | 0.115 | 0.115 | 1.03 | — |
| fixture 10k, before, `MALLOC_ARENA_MAX=2` | 0.101 | 0.109 | 0.113 | 0.115 | 1.06 | — |
| fixture 10k, after | 0.103 | 0.108 | 0.108 | 0.107 | 0.99 | — |
| fixture 10k, after, `MALLOC_ARENA_MAX=2` | 0.102 | 0.105 | 0.105 | 0.105 | 1.00 | — |
| Moana, before (recorded) | 30.6 | grew to about 44 over a few re-commits | | | | |
| Moana, after | 30.69 | 31.36 | 31.60 | 31.78 | 1.013 | 31.63 / 31.89 / 32.07 |

The growth was two scenes held at once and kept decodes never let go
(`interactive-updates` D6, M1 and M2): `SceneDelegate::commit_afresh` drops the old
scene before the fresh commit reads a prim (pinned by `a_recommit_holds_one_scene`),
and the kept assets are swept after every commit from scratch. A re-commit's peak is
now about 0.3 GiB above where it ends. Heaptrack on the fixture's loop finds no growing
allocation site (peak heap 83 MB, 1.8 MB never freed, all statics), and
`MALLOC_ARENA_MAX=2` changes nothing measurable: no allocator retention to answer, so
no allocator was adopted (Q4 not triggered). The 0.2 GiB a Moana re-commit still adds
is below what this method attributes.

### Moho gaps found

- **G1** stands: a linked light costs 3.16 s in `apply_pending_updates` on the island
  (Moho's light linking filter re-evaluating collections), now the whole of that
  edit's latency.
- **G2** did not arise: a camera edit, read through a counting index, reads no mesh,
  material or light prim (`a_camera_edit_reads_no_mesh_material_or_light`).
- **G3** did not arise: a prototype mesh's dirty reaches the delegate at the Hydra
  prototype path, for a native instance and a point instancer.
- **G4**: no Moho cache growth attributable (above).
- When a light with a non-trivial `shadowLink` is added, `categories` is dirtied on
  every prim the collection matches (the root, `/Render`, materials, lights), not only
  on receivers. This is Hydra parity: USD 26.08's `HdsiLightLinkingSceneIndex` sends
  the same notices, since it invalidates the union of matches without a type filter.
  Crust ignores the non-receivers. On a large stage the batch grows with the stage,
  and the walk evaluating the expression is the cost, tracked in doubleailes/Moho#27.
