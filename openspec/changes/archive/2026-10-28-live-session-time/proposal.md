## Why

Mantle (https://github.com/doubleailes/Mantle) gains a timeline: its session
has a current time, and its IPR should render the stage there. Mantle keeps
every time sample on the stage it writes and only says *which* time to
render, as a DCC hands a frame to husk; resolving the stage at that time is
the renderer's job. `crust render --frame t` and the import's time code
already do it offline, but the live session a host drives
(`LiveSession`, `live-session-host`) has no time: every import renders the
stage's default values, and an animated cache renders frozen.

## What Changes

- **`LiveSession::set_time(Option<f64>)`**: the time code the session renders
  at, as `crust render --frame` sets it (time samples resolve there and the
  sampler's frame seed follows). `None` keeps today's default values. It
  takes effect at the next `load` or `apply`; a time that is not finite is
  refused.
- **A time change is applied by the next `apply`.** The session imports with
  `crust check`'s own arguments plus `--frame t`, so a frame it renders at `t`
  is `crust render --frame t`'s by construction. A change of time alone is
  `full` in this first version: an in-place update evaluates at the time its
  scene was imported at (the provenance's options), so another time is
  another scene. Edits at an unchanged time still apply in place.
- **`Snapshot::time`**: every image reports the time code its render
  resolved the stage at, so a host that changed the time knows whether the
  image shows the new one yet.

## Capabilities

### Modified Capabilities

- `mcp-session`: the host session renders at a time code it is given.

## Impact

- `crust-render/src/session/`: `live.rs` (`set_time`, `Snapshot::time`), the
  frame threaded through `scene::import` / `Imported::update` /
  `import_kept` / `update_or_import` into `check_args`. `crust mcp` and the
  Hydra delegate pass no time and behave as before.
- Follow-up, not in this change: applying a time change in place, re-evaluating
  only the prims whose attributes are time-sampled through the in-place paths
  edits already use.

## Superseded by `moho-hydra-parity`

Archived with `--skip-specs`: its deltas target `mcp-session`, which `moho-hydra-parity`
retires.
- **Superseded:** `LiveSession::set_time`, and a time change as `full`.
- **Survives:** `Snapshot::time`. The time is now the host's index time, set on its stage
  scene index and applied in place when the class table allows (this change's follow-up,
  reached there).
