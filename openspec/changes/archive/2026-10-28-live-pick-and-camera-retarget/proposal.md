## Why

Mantle (https://github.com/doubleailes/Mantle) drives crust as its IPR through
`LiveSession` (`live-session-host`). Two things its viewer needs are missing.

- **Picking.** Clicking the image should select the prim under the cursor.
  `crust mcp`'s `probe` already names the prim and material a pixel's camera
  ray hits (`Imported::hit` + `Imported::names`, refused once the geometry was
  imported again), but a `LiveSession` host has no way to ask.
- **Switching cameras.** Mantle writes the IPR's camera as `rel camera` on the
  render settings prim (and on the products it names) in a strongest
  `ipr.usda` sublayer, and calls `apply` after rewriting it. A move or lens
  change of the render camera applies in place (`live-camera-update`), but
  changing *which* camera renders is a relationship change, which the
  classifier sends to `full`: a re-import per camera switch.

## What Changes

- **`LiveSession::pick(x, y) -> Result<Option<Pick>, Error>`**: answered on
  the session thread with the current render's camera and image size, for a
  pixel of the snapshot (rows down, in the render's region). `Pick` names the
  render, the hit's `geom_id`, the prim (the instance or instancer prim for a
  hit inside one) and the bound material. `Ok(None)` for a miss. `Error::Busy`
  when the scene no longer holds the geometry the render drew.
  `Imported::name_hit` is the one cast-and-name with that refusal, shared with
  `probe`.
- **A camera retarget is a `settings` edit.** The targets of `camera` on the
  `RenderSettings` prim the render uses, or on a `RenderProduct`, are applied
  in place through the settings rebuild, under the guards of
  `live-camera-update` and three more: the new camera is one the import's
  traversal met, no imported prim lies below it, and its shutter is the old
  one's. Without `crust:dicingCamera` the render camera dices, so a retarget
  on a scene with adaptive subdivision or on-screen-sized implicit shapes
  stays `full`. Every other relationship stays `full`.
- **No render or import change.** A fresh import is untouched; the in-place
  scene is that import's, bit for bit.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`: "Edits apply in place when they are confined" gains the
  camera retarget in the `settings` class; a new requirement, "A host names
  what a pixel shows", for `LiveSession::pick`.

## Impact

- **`crust-core`**: `live.rs` (`StageFacts::cameras`, `SettingsUpdate`'s
  camera path and shutter, `Update::apply` naming the new camera),
  `scene/usd_import/live.rs` (`refresh_settings` resolves and guards the new
  camera), `scene/usd_import/mod.rs` (records the cameras met); tests in
  `tests/live.rs`.
- **`crust-render`**: `session/delta.rs` (`LayerChange::Targets`, `classify`),
  `session/scene.rs` (`Imported::name_hit`, `Stale`), `session/live.rs`
  (`pick`, `Pick`), `mcp/render.rs` (`probe` through `name_hit`); tests in
  `session/live_tests.rs` and `mcp/tests.rs`.
- **Docs**: `openspec/specs/mcp-session/design.md` (probe and pick, the
  classification table, retargets, the settings rebuild),
  `openspec/specs/usd-scene-import/design.md` (the dicing camera),
  `docs/architecture.md` (`LiveSession`),
  `site/content/docs/help/claude-desktop.md` and
  `site/content/docs/architecture/limitations.md` (the in-place classes).
- **Ordering**: the spec delta modifies the requirement as
  `live-geometry-edits` leaves it (that change is implemented but not
  archived); archive that one first.

## Superseded by `moho-hydra-parity`

Archived with `--skip-specs` at 9/10 (task 5.3 dropped): its deltas target `mcp-session`,
which `moho-hydra-parity` retires.
- **Superseded:** its `LiveSession` and `probe` parts (`crust mcp` is removed).
- **Survives:** `pick`, in `render-delegate`'s render control. The camera retarget becomes
  a `settings` update from `renderProducts`.
