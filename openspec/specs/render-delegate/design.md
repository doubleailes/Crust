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
