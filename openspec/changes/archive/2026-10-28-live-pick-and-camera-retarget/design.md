## Context

- `probe` (`mcp/render.rs`, `hit_names`) casts the pixel-centre ray of a job's
  own camera and image size (`Imported::hit`), names the hit from the
  provenance (`Imported::names`), and refuses once `Imported::geometry` — the
  import that built the geometry, kept by in-place updates that move none —
  is not the job's.
- `LiveSession` runs one render at a time from the session thread's scene;
  `start` builds a `Running` per render, and the scene it renders is the
  session's `scene` until the next `load` or `apply`.
- The settings rebuild (`refresh_settings`) re-opens the stage, re-reads the
  settings, products and camera through the import's readers, builds the camera
  with `build_camera`, and refuses (`Structural`) whatever would move what the
  import baked. It refused any change of the camera a fresh import would render
  through ("the render camera changed").
- The classifier (`session/delta.rs`) reported a relationship change as
  `LayerChange::Other`, which is `full`.

## Goals / Non-Goals

**Goals:**
- A host names the prim and material under a pixel of the image it shows,
  from any thread, never from the wrong scene.
- A camera switch as Mantle authors it is a `settings` update, bit-identical
  to a `full` application.

**Non-Goals:**
- Picking with another ray (a lens sample, a time in the shutter) or through
  opacity: the pick is `probe`'s.
- Retargeting the dicing camera in place: it moves geometry.
- Any other relationship in place (material bindings, light links, products'
  `orderedVars`).

## Decisions

### D1. One cast-and-name, `Imported::name_hit`

`name_hit(rendered, camera, size, pixel) -> Result<Option<HitNames>, Stale>`
does the cast, the naming and the staleness refusal; `probe` maps `Stale` to
its note, `pick` to `Error::Busy`. A copy in `live.rs` would let the two drift
(the image-row flip, the refusal).

### D2. The pick reads the running render, on the session thread

`Running` keeps the camera (`Renderer::camera`), the frame size and region
(`Renderer::settings`) and `Imported::geometry` at `start`. `pick` runs as a
command, so it waits behind a `load` or `apply` and then describes the render
that started; the scene is only read on the thread that owns it. Its pixel is
the snapshot's (rows down, region-local), mapped to the frame for the ray.
`Pick::render` names the render it describes, so a host still showing an older
render's image can tell. `Busy` when the session holds no scene (a failed load,
a panic) or another geometry import than the render's.

*Alternative considered:* answering from the polling thread through a shared
`Arc<Imported>`. The renderer is shared already, but the provenance and the
slot map are not, and sharing them would change what every import hands the
render thread; a pick is not on a per-frame path.

### D3. A relationship's targets are their own change

`layer_diff` reports `LayerChange::Targets(path)` for a relationship spec
whose `targetPaths` changed (or which appears or goes with only its targets,
`custom` and `variability`). `classify` admits `camera` on the settings prim
the import read, or on a prim whose composed type is `RenderProduct` (the
first product's camera wins over the settings', and the rebuild re-reads every
product), as `settings`; any other `Targets` is `full`, as before.

### D4. The rebuild resolves the camera a fresh import would use, and guards it

`refresh_settings` resolves the target as the import does: the camera named
(the host's, the first product's or the settings'), else the first camera the
traversal met. `StageFacts::cameras` records every camera the traversal met,
in order. A target other than the scene's camera is refused unless:

- it is in `cameras` (a fresh import builds a named camera only when its walk
  meets it, else falls back to the first with `camera.not_a_camera`);
- `Provenance::any_below(target)` is false (the rebuild re-reads only the
  camera);
- its `ShutterKey` is the scene's with the camera replaced (geometry was posed
  over the old shutter).

The dicing keys need no new code: with no `crust:dicingCamera` the dicing
camera is the render camera, so a retarget moves the adaptive or on-screen key
and is refused; with one, the keys stand. `SettingsUpdate` carries the new
camera's path and shutter key, and `Update::apply` writes them into
`Scene::camera_path` and the provenance, so the next classification and
refresh read the new camera.

With nothing named, the rebuild used to keep the camera it had. A removed
`camera` relationship is now a `settings` edit, and a fresh import then renders
through the first camera met, so the rebuild resolves that one instead.

*Alternative considered:* recording only the first camera met. A retarget to
any other camera would then need the stage walked again to know the import
would meet it.

## Risks / Trade-offs

- [Something the import reads from the render camera other than its frame,
  exposure and shutter is missed, so a retarget in place diverges from `full`]
  → bit-identity tests at 16 spp: a mesh stage with a product camera, its
  undo, a move of the new camera, an adaptive stage diced elsewhere, and the
  Mantle layer shape in the live session.
- [`cameras` grows with the stage's camera count] → one path per camera; no
  stage keeps many.

## Migration Plan

None: unauthored stages import as before. A host that never retargets or picks
sees no change.
