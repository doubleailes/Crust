# render-delegate — design record

> Design record for the **render-delegate** capability: Crust as a Hydra 2.0 render
> delegate, handed a scene index by its host and committed from it. The reasoning,
> measurements and history behind the behaviour `spec.md` states.
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.
> The capability was founded by the change `moho-hydra-parity` (its design D1–D14). Two
> parts come from the retired `hydra-delegate` record of hdCrust, the C++ Hydra plugin
> that change removed: how a Hydra host hands a delegate its scene index, and how a panic
> is contained.

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

## What the host is asked, and what the image depends on

A Hydra delegate is handed a terminal scene index and nothing else: UsdImaging
resolves asset paths before they reach it, and it never sees a stage. Crust's delegate
asks its host one question about the stage (`HostFacts`): the `.mtlx` document a
material references, which openusd does not compose, so Crust reads the file itself
through `HostFacts::mtlx_reference` until `mtlx-format-plugin` composes it into the
stage. Everything else it reads, warns about or reports comes from the index.

Until `host-facts-mtlx-only` it asked eight more questions, facts that are not scene
description: whether a time range is authored, the composed layers and the root layer
stack, the `invisible` overrides, why a pass is missing, a material's foreign spec
paths, the transform facts behind three `xform.*` warnings, and whether an instance
lacks a prototype. `hydra-only-renders` had already made them diagnostics only; that
change removed them. The time range is read from the index's `sceneGlobals`, which
gives `0` for an end left unauthored and does not say which ends are: `0..0` and a
start after its end (a start authored alone) are no range, so a stage authoring `0..0`
itself gets no `time.outside_range`, and an end authored alone reads as `0..end`; a missing pass is explained from the index (no prim,
or its type as the index's prim info names it); the `xform.*` and `instancing.instanceable_without_prototype`
warnings went, the index carrying none of their causes (`crust-check/3`); and the
stamp's layer stack and override count are set by the batch host, from the stage it
opened, after the commit.

Two facts that used to change the image moved onto the index. The stage unit, which
the lens converts its millimetres through, is Moho's `__usdMetersPerUnit` on the
absolute root (Hydra carries no unit; UsdImaging's camera scales lens values by a
fixed 0.1). Asset paths are the index's `asset` values, which Moho resolves on their
authoring layer as UsdImaging does; only a path authored as a `string` lost its layer
anchoring, and it now resolves against the stage's directory with `asset.string_path`.

The guard is `crust-render/tests/host_facts.rs`: every sample that references no
`.mtlx` renders bit-identically at 16 spp with `NoHostFacts` and with
`StageHostFacts`, and a `.mtlx`-referencing one is checked to differ, so the list of
exceptions cannot hide a regression. `cornellbox_guided` is left out too: path guiding's
training is not bit-reproducible run to run (about one render in five differs on the same
binary), so two renders of it say nothing about the host's facts.

## Panics

hdCrust built under an unwinding profile and caught a panic at every boundary
(`hydra-render-delegate` D8). The CLI's `release` profile aborts instead: nothing in the
renderer depends on unwinding, and dropping it removes the landing pads around the
`expect()`s and the indexing in the bounce loop (`Cargo.toml`). `crust render`'s goldens
and instruction counts are those of `abort`. A host that wants containment sets
`panic = "unwind"` in its own workspace and catches around its call to
`SceneDelegate::commit` and the render.

## History: the live delegate (removed by `remove-live-delegate`)

From `interactive-updates` to `remove-live-delegate` (2026-11-11), the delegate was kept
by a live host, Mantle's IPR, and brought up to date from its index's notices:

- **Update classes** (`unchanged`, `settings`, `materials`, `lights`, `geometry`,
  `full`), decided from the batch's added and removed prims and dirtied locators.
- **In-place rebuilds** (`scene/hydra/update.rs`) through the commit's own readers, against
  a `Provenance` every import recorded (which world slots hold a material, where each
  light went, each mesh placement and prototype, the settings the commit read). The world
  could keep a copy of its geometry (`keep_inputs`) to replace meshes and the light layer,
  and an update applied in place was pinned bit-identical to a fresh commit
  (`crust-core/tests/live.rs`).
- **Render control** (`crust-render`'s `delegate` feature): restart on update, progress
  snapshots, cancel and pick, with a panic reported once.
- **Kept assets**: `FileAssets::keep_decoded`, revalidated against file stamps before each
  update and swept after it, and environment maps shared across commits.
- **Placement tools** (`crust_core::place`): drop and scatter against the committed world.

Mantle was cancelled, which left Crust's command line as its only host. It commits once,
so all of the above had no caller, while every import still paid for the provenance and
every change to the import had to keep its in-place twin bit-identical. The light layer's
own kernel segment, which existed so the layer could be rebuilt alone, was kept: removing
it changes the BVH, and so the order of exact ties, in every scene with a light. Measured
update latencies (1 000- and 10 000-mesh fixtures, Moana) are in this record's history
before that change.
