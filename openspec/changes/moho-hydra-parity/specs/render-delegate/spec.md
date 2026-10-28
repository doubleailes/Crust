## Purpose

Crust as a Hydra 2.0 render delegate. A host hands Crust a scene index, Crust keeps
its scene up to date from the index's notices, and the host drives and polls the
render.

## ADDED Requirements

### Requirement: A host hands the delegate a scene index

Crust's delegate SHALL take its scene from a scene index that a host gives it. In
this change, that is Moho's `chain::standard` over a stage the host opened. The
delegate SHALL add Moho's light linking and dependency forwarding renderer filters,
batch the index's notices by prim type, and observe the batches with a prim managing
observer. Each of the host's `update` calls SHALL flush the batch and commit the
scene. The delegate SHALL own no stage, open no file and read no layer. Stage edits,
time changes and loading are the host's.

`crust render`, `crust check` and `crust ls` SHALL be the batch host: each opens the
stage, builds the chain, makes one delegate, and updates it once.

#### Scenario: The batch host

- **WHEN** `crust render -i samples/cornellbox.usda -s 16` runs
- **THEN** the scene rendered is the one the delegate committed after one update of
  the chain over the opened stage

#### Scenario: A host-driven edit

- **WHEN** a host opens a stage, builds a delegate, sets a light's `inputs:intensity`
  on the stage, calls `apply_pending_updates` on its stage scene index and then the
  delegate's `update`
- **THEN** the delegate's next render uses the new intensity, and the host opened no
  file for it

### Requirement: Updates apply in place from the dirty locators

At each `update`, the delegate SHALL decide from the batch alone how to bring its
scene up to date: the prims added and removed, by type, and the dirtied locators. It
SHALL report the decision as one of these classes, cheapest first:
- `settings`: render settings, products or vars dirtied, or the render camera's
  `camera` or `xform` dirtied, when nothing the committed geometry, lights or world
  were built from moves. A settings or camera edit SHALL be `full` when it moves
  any of these:
  - the projection that tessellation reads: the dicing camera's position,
    orientation, focal length or aperture, the resolution, or the adaptive target
    or level ceiling (the dicing camera is `crust:dicingCamera`, else the render
    camera);
  - the subdivision level or the working colour space;
  - the shutter that motion was measured over: the camera's `shutter:open` or
    `shutter:close`, the settings' `shutterInterval`,
    `disableMotionBlur` when moving placements were posed at the frame, or a
    retarget to a camera whose shutter differs;
  - the dome's camera visibility (`domeLightCameraVisibility`);
  - the primvars the products ask the geometry to keep;
  - a precomp, before or after the edit.
  Changing which camera renders is `settings` when the new camera is one the scene
  committed, carries no committed prim below it, and keeps the shutter.
- `materials`: a used material's `material` dirtied, when the material's opacity,
  transmission and emission class does not move.
- `lights`: a light's `light` or `material` dirtied; a light added or removed,
  whatever its collections; a distant or dome light's `xform` dirtied; a receiver's
  `categories` or an instancer's `instanceCategories` dirtied.
- `geometry`, under the conditions the `live-geometry-edits` design set for it:
  - a mesh's `xform` or points dirtied;
  - a mesh's topology or primvars dirtied, when the mesh is placed once, not
    displaced, does not become identical to another mesh, and subdivision is not
    adaptive;
  - an area light's `xform` or shape dirtied.
- `full`: anything else. A fresh delegate is committed from the index's current
  prims.

A render after an update applied in place SHALL be bit-identical to the render of a
fresh delegate over the same index. With `CRUST_SCENE_DELTAS=0`, every update SHALL be
`full`.

#### Scenario: A shader input

- **WHEN** a host sets `inputs:base_color` on a shader of a material in use and updates
- **THEN** the update reports `materials`, and a 16 spp render is bit-identical to
  `crust render` of the edited stage

#### Scenario: Adding a light with trivial collections

- **WHEN** a host defines a `SphereLight` with no `collection:lightLink` properties and
  updates
- **THEN** the update reports `lights`, and a 16 spp render is bit-identical to a fresh
  delegate's

#### Scenario: Adding a light linked to one prim

- **WHEN** a host defines a `RectLight` whose `collection:lightLink` includes one mesh,
  and updates
- **THEN** the update reports `lights`, only that mesh is lit by the new light, and a
  16 spp render is bit-identical to a fresh delegate's

#### Scenario: A mesh's transform

- **WHEN** a host sets `xformOp:translate` on a mesh placed once and updates
- **THEN** the update reports `geometry`, and a 16 spp render is bit-identical to a
  fresh delegate's

#### Scenario: A shutter change

- **WHEN** a scene with time-sampled transforms renders with motion blur, and a host
  sets `shutter:close` on the render camera from 0.5 to 0.25 and updates
- **THEN** the update reports `full`, and a 16 spp render is bit-identical to a fresh
  delegate's

#### Scenario: Exposure is a settings edit

- **WHEN** a host sets `exposure` on the render camera and updates
- **THEN** the update reports `settings`, and a 16 spp render is bit-identical to a
  fresh delegate's

#### Scenario: Focal length on an adaptively subdivided scene

- **WHEN** the scene sets `crust:subdivisionEdgeLength`, and a host sets `focalLength`
  on the render camera and updates
- **THEN** the update reports `full`

#### Scenario: A prim added

- **WHEN** a host defines a `Mesh` and updates
- **THEN** the update reports `full`, and the mesh renders

### Requirement: A time change is an index time change

The host SHALL set the time on its stage scene index. The delegate SHALL treat the
dirtied locators that the time change sends like any other update, and SHALL apply it
in place when the classes above allow. Every image SHALL report the time of the
index the scene was committed from.

#### Scenario: A camera that moves over time

- **WHEN** only the render camera's transform is time-sampled, and the host moves the
  index's time
- **THEN** the update reports `settings`, and the snapshot reports the new time

### Requirement: The host drives and polls the render

The delegate SHALL render on its own threads, and expose to the host:
- starting a progressive render of the committed scene, at a sample count and region;
- a restart after each update that changed the scene: the running render is
  cancelled and joined, and a fresh one started;
- `cancel`, which stops and joins the render, and `cancel_render`, which signals it to
  stop from any thread without waiting;
- `render`, the id of the render last started;
- `generation`, which moves when a newer image is published;
- `snapshot`, which returns the latest beauty, its samples per pixel, the render id,
  the time, and whether it is the completed render's final image;
- `converged` and the render's outcome;
- `aov`, which returns a named AOV once the render completed;
- `pick`, which names the prim and material under a pixel of the current render.

In a build that unwinds on panic (the host's choice of profile), a panic in the
delegate or a render SHALL be returned to the host as an error, never unwind into
it. The CLI's release profile aborts on panic, as before. The decoded assets (images, texture tiles, Ptex) SHALL be kept across
updates, and an asset whose file changed on disk SHALL be decoded again.

#### Scenario: Restart on update

- **WHEN** a render is refining and the host updates with a light edit
- **THEN** the running render is cancelled, `render` moves to a new id, and the next
  snapshot names the new render

#### Scenario: Cancel without waiting

- **WHEN** an update is being committed and the host calls `cancel_render`
- **THEN** the call returns at once

#### Scenario: Agreeing with the CLI

- **WHEN** a delegate renders a stage at 16 spp to completion, and the user runs
  `crust render -i <stage> -s 16`
- **THEN** `crust diff` reports the two images identical

#### Scenario: A pick

- **WHEN** the host picks a pixel of the current render that shows a mesh
- **THEN** the pick names that mesh's prim and its bound material

### Requirement: Update latency is reported, not gated

Crust SHALL provide an update-latency benchmark that times four edits on a delegate
over Moho:
- a camera's `focalLength`;
- a camera's transform;
- adding a light with trivial collections;
- adding a light linked to one prim.

Each edit SHALL be timed from the edit on the stage to the restarted render: from
authoring the edit, through `apply_pending_updates` and the delegate's update, to the
new render having started. The benchmark SHALL report, per edit, the median and worst
time over N runs, and the split between authoring, the scene index's update (up to
the managing observer), Crust's commit and the render restart. It SHALL also report
the class the edit was applied as. It SHALL run on a generated fixture of N meshes
and on a named stage (Moana). Its results SHALL be recorded in the `render-delegate`
design record, and SHALL NOT gate any check.

#### Scenario: The fixture

- **WHEN** the benchmark runs on a fixture of 10 000 meshes
- **THEN** it prints one row per edit with its median, worst, split and class, and the
  camera `focalLength` row reports `settings`

#### Scenario: Not a gate

- **WHEN** an edit's median time is higher than the recorded figure
- **THEN** the benchmark still exits 0
