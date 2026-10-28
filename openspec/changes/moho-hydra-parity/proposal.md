## Why

Moho, the scene index Crust imports through, has gone to strict Hydra 2.0 /
UsdImaging parity (Moho's `hydra-parity-stage`, `-instancing`, `-materials`,
`-chain` and the final check, merged at `510174c`). It no longer streams. It lays
out instancing as UsdImaging does, it has dropped M10 and M12, and it resolves
materials and bindings as UsdImaging does. It moved light linking and dependency
forwarding into renderer filters, and it offers the Hydra 2.0 consumer path: notice
batching by prim type and a prim managing observer. Crust pins Moho at `d09cc94` and
still depends on everything that was dropped.

The parity work had a second purpose: Crust should be a Hydra 2.0 render delegate,
in the strict sense. It is handed a scene index by its host and updates its scene in
place from the dirty locators, as hdPrman's Riley path does through
`SetTerminalSceneIndex` and `Update`. The same Crust code runs over Moho in Rust
today, and its shape lets a later host (C++ or other) hand it another scene index.
Crust's layer-file session plumbing, its MCP server and its layer-writing Hydra
plugin (hdCrust) predate that model, and they go. A Hydra host integration will be re-architected later, out of scope.

S2 (`moho-import`) also grew the codebase by about 4 700 lines of code and 74 000
lines of baselines, to prove parity with an importer that no longer exists. This
change is where the code shrinks. Heavy breaking changes are accepted.

## What Changes

- **Moho bump** to `510174c` (Moho main). openusd still comes only through
  `moho::openusd`.
- **Crust becomes a Hydra 2.0 render delegate.**
  - A host hands it a scene index.
  - It adds `chain::renderer_filters(index, &[RendererFilter::light_linking(),
    RendererFilter::dependency_forwarding()])`, batches notices by prim type
    (`PrimTypeNoticeBatchingSceneIndex`), and observes the batches with Crust's prim
    factory (`moho::prim_managing`).
  - Each `update` (flush) commits the scene in place from the dirty locators, or
    afresh when it cannot prove the change confined.
  - It owns no stage and reads no layer file.
- **The batch host.** `crust render`, `check`, `diagnostic`, `light-paths` and `ls`
  each open a stage, build `chain::standard` over it, make one delegate and update it
  once. **BREAKING**: `crust ls` now lists from the index by Hydra prim type.
- **Render control.** Render control is what remains of `LiveSession`: the render
  thread, cancel and restart, and what a host polls (`render`, `generation`,
  `snapshot`, `aov`, `pick`, `converged`, the outcome, the time). It becomes the
  delegate's render-control API, the analogue of a render pass and its render
  buffers.
- **BREAKING: the layer plumbing is removed.** This covers owned layers,
  `session/sources.rs`, `session/layer.rs`, `session/delta.rs` (layer diff and
  classify), `load`, `load_owned` and `apply` following files on disk,
  `set_time` on a session, and the masked-stage rebuilds (`usd_import/live.rs`,
  `crust_core::live`'s `Change` and `Update`).
- **BREAKING: `crust mcp` is removed**, with the `mcp` feature, its dependencies
  (`rmcp`, `tokio`, `schemars`, `base64`), the MCP Bundle packaging and its nightly
  job. The `mcp-session` capability is retired. The MCP server becomes a Mantle
  feature, a follow-up in Mantle outside this change.
- **Crust reads only Hydra schemas**, never Moho's generic data (`__usdAttributes`,
  `__usdRelationships`). **BREAKING** for scenes:
  - geometry and volume `crust:*` attributes become constant primvars
    (`primvars:crust:*`);
  - render settings, product and var `crust:*` attributes are read from Hydra's
    `namespacedSettings`;
  - `crust:*` relationships on those prims become `string` path attributes, and
    `crust check` warns when such a path crosses a reference or payload
    (`settings.path_across_arc`);
  - a UsdLux light's `crust:rayMask`, `crust:light:*` and
    `primvars:ri:attributes:visibility:camera` stay attributes on the light, read by
    name through the `light` container (UsdImaging's `LookupLightParamAttribute`
    answers any attribute name);
  - precomp filters become `Material` prims with `outputs:crust:*` terminals.
  `crust check` also reports legacy `crust:*` authoring (`attribute.legacy_crust`).
- **Single-stage import.** **BREAKING**: `CRUST_STREAM_IMPORT` is removed, along
  with the streamed chunks, the side stages and the chunk inputs. Crust keeps no
  chunking.
- **Hydra's instancing layout.** Native prototypes are read at
  `UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer/UsdNiPrototype`,
  and point instancer prototypes from their `ForInstancer<hash>` copies. With M12
  gone, Moana's `isCoral` coral prototypes are empty, as in UsdImaging 2.0. This is a
  listed parity exception against the old direct importer. Adaptive placements are
  counted over the whole stage.
- **Light linking from Hydra's categories** (`group_<n>`, `categories`,
  `instanceCategories`).
- **Missing means default.** An absent `xform` is the identity, an absent
  `visibility` is visible, and any other absent flattened data takes its default.
- **Time is the index's.** **BREAKING**: without `-f`, the stage is read at time code
  0, as Hydra reads it. `crust check` warns on each attribute that authors both a
  default and time samples (`time.default_and_samples`). A time change in a kept
  delegate is an index time change.
- **Parity machinery removed.** This covers the baselines (3 603 files),
  `parity.rs`, the `parity` feature and its digests,
  `crust-render/tests/import_parity.rs`, `scripts/import_parity.sh`,
  `scripts/parity_images.sh` and its expected list. The `import-parity` capability
  is retired. The image goldens (`scripts/check_images.sh`) remain the guard.
- **Update latency is measured, not gated.** A benchmark times four edits, each
  from the edit on the stage (through `apply_pending_updates`) to the restarted
  render, on a fixture and on Moana:
  - a camera's `focalLength`;
  - a camera's transform;
  - a light added with trivial collections;
  - a light added and linked to one prim.
  First-load time is re-measured.
- **BREAKING: hdCrust is removed.** This covers the C++ Hydra render delegate and
  its tests under `hydra/` (with its pixi environment), the `crust-hydra` crate, and
  `.github/workflows/hydra.yml`. It also covers what exists only for them:
  `[profile.hydra]`, the `.gitignore` entries, the cookbook recipe, and the site's
  `help/usdview.md` page and Hydra limitations. The unarchived `hydra-render-delegate`
  change and the `hydra-delegate` design record are retired. A Hydra host integration will be re-architected later, out of scope.
  The render-delegate shape (a host hands Crust a scene index, and Crust is a prim
  factory behind the managing observer) is what such a host will sit on.
- **Mantle's Crust integration breaks.** It used `LiveSession`'s layer API. It is
  rebuilt on Mantle's side over a Hydra viewport scene index, outside this change.

## Capabilities

### New Capabilities

- `render-delegate`: Crust as a Hydra 2.0 delegate.
  - A host hands it a scene index, and `update` applies the batch in place from the
    dirty locators, with reported classes and bit-identity to a fresh delegate.
  - Time changes go through the index.
  - The render-control API: render, restart, cancel, snapshot, AOVs, pick.
  - The update-latency benchmark.

### Modified Capabilities

- `scene-index-import`: the import is the delegate over Moho, reading only Hydra
  schemas, through the batch host. Other changes:
  - one stage, time code 0;
  - Hydra parity with listed exceptions;
  - renderer attributes carried as Hydra carries them, and missing data read as its
    default;
  - the streaming and direct-importer parity requirements removed.
- `import-parity`: retired. Every requirement is removed.
- `mcp-session`: retired. Every requirement is removed: `crust mcp` goes, and what
  survives of in-place updates and render control is specified in
  `render-delegate`.
- `cli`: no `mcp` subcommand. `crust ls` lists from the batch host's index, and
  without `-f` it lists values at time code 0.
- `lighting`: a light's renderer attributes (`crust:rayMask`, `crust:light:*`, the
  RenderMan camera-visibility primvar) are read from its Hydra `light` data, never
  as `primvars:crust:*`.
- `usd-scene-import`:
  - loading, light linking, volumes, native nesting and unread types are read through
    Hydra;
  - no streaming in geometry mapping, light mapping or adaptive subdivision;
  - adaptive placements are counted over the whole stage.
- `displacement`: determinism without streaming.
- `render-passes`: pass collections from Hydra's `collections`, and no streamed-chunk
  gap.

## Impact

- **Code:**
  - `crates/crust-core/src/scene/usd_import/`: becomes the Moho batch host and the
    host facts.
  - A new `crates/crust-core/src/scene/hydra/`: the delegate (factory, managed prims,
    commit, family readers).
  - `crates/crust-core/src/live.rs`: keeps `Provenance` for picks.
  - `crates/crust-render/src/session/`: replaced by `crates/crust-render/src/delegate/`
    (render control, feature `delegate`).
  - `crates/crust-render/src/mcp/` and `crates/crust-render/tests/mcp.rs`: deleted.
  - `hydra/` (hdCrust, 25 files with `pixi.lock`), `crates/crust-hydra/` (8 files)
    and `.github/workflows/hydra.yml`: deleted; `[profile.hydra]` is removed from
    `Cargo.toml`.
  - `Cargo.toml`, `Cargo.lock`, `deny.toml`: Moho rev, MCP dependencies dropped.
  - `packaging/mcpb/` and the nightly MCP job are deleted.
- **Samples and fixtures:** every `crust:*` geometry or volume attribute moves to
  `primvars:crust:*`, and `samples/precomp.usda` moves to `Material` filters and
  string paths. This affects the open `add-precomp-filters` change (design,
  Migration Plan).
- **Docs:**
  - `CLAUDE.md` and `docs/architecture.md`: crates, render flow, the Invariants pairs,
    switches, and the removed `crust mcp`.
  - Design records: `usd-scene-import`, a new `render-delegate` (absorbing what
    survives of `mcp-session`'s), and `cli`.
  - Site: `usd/*`, `reference/*`, `architecture/usd-import.md`,
    `architecture/overview.md`, `architecture/limitations.md`; delete
    `help/claude-desktop.md` and `help/usdview.md`.
  - Delete `openspec/specs/hydra-delegate/design.md`.
- **Other repositories:** Mantle's Crust integration breaks, and Mantle gains the MCP
  server later. Both are outside this change.
- **Performance:**
  - Render throughput is unchanged.
  - Path-ordered ids move images at noise level once.
  - First load is re-measured on ALab and Moana.
  - Without streaming, Moana composes whole (Moho: a 7.01 GiB peak).
- **Size:** net about −20 700 lines (±2 500) of Rust, C++, scripts and workflows, plus
  `hydra/pixi.lock` (4 306 lines), and −74 155 lines
  (−23 MB, −3 603 files) of baselines (design, "Size").
