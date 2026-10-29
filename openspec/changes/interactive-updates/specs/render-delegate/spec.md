## MODIFIED Requirements

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
  - a mesh's points, normals, topology or primvars dirtied, when the mesh is not
    displaced, does not move over the shutter, subdivision is not adaptive, and its
    new shape is identical to no other distinct mesh of the scene. This holds whether
    the mesh is placed once, shares its distinct mesh with other direct placements,
    or is a part of a native-instance or point-instancer prototype that no nested
    instancer places;
  - an area light's `xform` or shape dirtied.
- `full`: anything else. A fresh delegate is committed from the index's current
  prims.

Every update of class `settings`, `materials`, `lights` or `geometry` SHALL be applied
in place, without committing the scene afresh, unless one of these holds, in which
case it SHALL be committed afresh and reported `full`:
- a guard above;
- an active render pass;
- a warning the scene carries about the prims the update rebuilds, or one the rebuild
  raises that a fresh commit might report differently.

This includes a light added or removed and a receiver's link categories: the light
sources, their light and shadow links, and the light selection are rebuilt, and no
other geometry is read again.

A render after an update applied in place SHALL be bit-identical to the render of a
fresh delegate over the same index, and the update SHALL report the warnings a fresh
delegate reports. With `CRUST_SCENE_DELTAS=0`, every update SHALL be `full`.

#### Scenario: A shader input

- **WHEN** a host sets `inputs:base_color` on a shader of a material in use and updates
- **THEN** the update reports `materials`, and a 16 spp render is bit-identical to
  `crust render` of the edited stage

#### Scenario: Adding a light with trivial collections

- **WHEN** a host defines a `SphereLight` with no `collection:lightLink` properties and
  updates
- **THEN** the update reports `lights`, the scene is not committed afresh, and a 16 spp
  render is bit-identical to a fresh delegate's

#### Scenario: Adding a light linked to one prim

- **WHEN** a host defines a `RectLight` whose `collection:lightLink` includes one mesh,
  and updates
- **THEN** the update reports `lights`, the scene is not committed afresh, only that
  mesh is lit by the new light, and a 16 spp render is bit-identical to a fresh
  delegate's

#### Scenario: Adding a shadow-linked light among shadow-linked lights

- **WHEN** a scene holds a light whose `collection:shadowLink` excludes one mesh, and a
  host defines a second light whose `collection:shadowLink` excludes another mesh, and
  updates
- **THEN** the update reports `lights`, the scene is not committed afresh, and a 16 spp
  render is bit-identical to a fresh delegate's

#### Scenario: Removing a light

- **WHEN** a host removes an area light that sits before another area light in path
  order, and updates
- **THEN** the update reports `lights`, the scene is not committed afresh, and a 16 spp
  render is bit-identical to a fresh delegate's

#### Scenario: A light linked to nothing comes back

- **WHEN** a scene holds a `RectLight` whose `collection:lightLink` includes no prim of
  the scene, so that it illuminates nothing, and a host adds a mesh to its
  `collection:lightLink:includes` and updates
- **THEN** the update reports `lights`, the scene is not committed afresh, the mesh is
  lit by the light, and a 16 spp render is bit-identical to a fresh delegate's

#### Scenario: A receiver's categories

- **WHEN** a host adds a mesh to a light's `collection:lightLink:includes`, so that
  Moho dirties the mesh's `categories`, and updates
- **THEN** the update reports `lights`, the scene is not committed afresh, the mesh is
  lit by the light, and a 16 spp render is bit-identical to a fresh delegate's

#### Scenario: A mesh's transform

- **WHEN** a host sets `xformOp:translate` on a mesh placed once and updates
- **THEN** the update reports `geometry`, and a 16 spp render is bit-identical to a
  fresh delegate's

#### Scenario: Reshaping a mesh another prim shares

- **WHEN** two mesh prims author the same points, topology and material, so that one
  distinct mesh is instanced twice, and a host moves one point of the first and
  updates
- **THEN** the update reports `geometry`, the scene is not committed afresh, and a 16
  spp render is bit-identical to a fresh delegate's

#### Scenario: Reshaping a prototype's mesh

- **WHEN** a mesh inside a prototype placed by several native instances, or by a
  point instancer, has its points edited, and a host updates
- **THEN** the update reports `geometry`, the scene is not committed afresh, every
  placement shows the new shape, and a 16 spp render is bit-identical to a fresh
  delegate's

#### Scenario: A reshape that makes two meshes identical

- **WHEN** a host edits a mesh's points so that they equal another mesh's, with the
  same topology and material
- **THEN** the update reports `full`

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

### Requirement: Update latency is reported, not gated

Crust SHALL provide an update-latency benchmark that times these edits on a delegate
over Moho:
- a camera's `focalLength`;
- a camera's transform;
- adding a light with trivial collections;
- adding a light linked to one prim;
- removing a light;
- moving an area light;
- reshaping a mesh placed once;
- reshaping a mesh inside a prototype placed more than once;
- adding a mesh to a light's `collection:lightLink:includes`.

Each edit SHALL be timed from the edit on the stage to the restarted render: from
authoring the edit, through `apply_pending_updates` and the delegate's update, to the
new render having started. For each edit, the benchmark SHALL report:
- the median and worst time over N runs;
- the split between authoring, the scene index's update (up to the managing
  observer), Crust's commit and the render restart;
- the class the edit was applied as;
- the process's resident memory after the edit.

It SHALL report the resident and peak memory after setup. It SHALL also offer a
re-commit loop that applies a `full` update K times and reports the resident memory
after each. It SHALL run on a generated fixture of N meshes, extended with an instanced
prototype, and on a named stage (Moana). Its results SHALL be recorded in the
`render-delegate` design record against the targets that record states, and SHALL NOT
gate any check.

#### Scenario: The fixture

- **WHEN** the benchmark runs on a fixture of 10 000 meshes
- **THEN** it prints one row per edit with its median, worst, split, class and resident
  memory, and the camera `focalLength` row reports `settings`

#### Scenario: Re-commits

- **WHEN** the benchmark runs its re-commit loop three times on the fixture
- **THEN** it prints the resident memory after setup and after each re-commit

#### Scenario: Not a gate

- **WHEN** an edit's median time is higher than the recorded figure or its target
- **THEN** the benchmark still exits 0

## ADDED Requirements

### Requirement: A settings update reads only the settings and cameras

A `settings` update SHALL read from the scene index only these prims:
- the render settings, products and vars;
- the active render pass;
- the render camera, the dicing camera and a retarget's new camera;
- the root's `sceneGlobals`.

It SHALL NOT walk the stage's layers. The work it does, and the work of restarting the
render after it, SHALL NOT grow with the number of prims, geometries or material slots
in the scene.

#### Scenario: The prims a camera edit reads

- **WHEN** a host moves the render camera of a scene of 1 000 meshes and updates,
  through a scene index that counts the prims read
- **THEN** the update reports `settings`, and no mesh, material or light prim was read

#### Scenario: Scene size

- **WHEN** the update-latency benchmark edits the camera's `focalLength` on fixtures of
  1 000 and 10 000 meshes
- **THEN** both report `settings`, and the benchmark prints both medians, so that a
  growth between them is visible

### Requirement: A re-commit holds one scene

When an update commits the scene afresh, the delegate SHALL release the scene it
replaces, its world, light list and the render that used them, before it reads the
index for the new commit. This SHALL hold on every path that commits afresh:
- an update classified `full`;
- an in-place class that a guard sends to a fresh commit;
- an in-place update whose rebuild or application is refused and falls back to a fresh
  commit (reported `full`). Decoded assets that the new commit does not ask for SHALL be
let go after it, as before a first commit. A failed re-commit SHALL leave the
delegate with no scene and the batch's notices pending, so the next update commits
afresh.

#### Scenario: The old world is gone

- **WHEN** a host adds a mesh (a `full` update) to a scene whose world a test holds
  only a weak reference to
- **THEN** the weak reference is dead before the new commit reads its first prim

#### Scenario: A refused in-place update falls back with one scene

- **WHEN** a scene sets `crust:dicingCamera` to its render camera, a test holds only a
  weak reference to its world, and a host sets `focalLength` on that camera and updates
  (a `settings` update whose rebuild is refused because it would move the dicing
  projection)
- **THEN** the update reports `full`, and the weak reference is dead before the fresh
  commit reads its first prim

#### Scenario: A guard sends a light edit to a fresh commit

- **WHEN** a scene renders an active render pass, a test holds only a weak reference to
  its world, and a host adds a light and updates
- **THEN** the update reports `full`, and the weak reference is dead before the fresh
  commit reads its first prim

#### Scenario: Re-commits do not accumulate

- **WHEN** the update-latency benchmark's re-commit loop runs three times on the
  10 000-mesh fixture
- **THEN** the resident memory after the third re-commit is reported beside the
  resident memory after the first, and the design record states the difference
