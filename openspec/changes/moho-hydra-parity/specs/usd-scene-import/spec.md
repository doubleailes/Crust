## MODIFIED Requirements

### Requirement: Load a scene from a USD stage

The importer SHALL open a `.usda`, `.usdc`, or `.usdz` file as one stage, populate
the delegate over it (see `scene-index-import`), and commit the scene once every prim
is synced: render settings and the camera first (so the camera can derive its aspect
ratio), then geometry, volumes and lights. World transforms SHALL be Hydra's
flattened `xform` (parent transforms composed by the scene index), relative to the
prototype root for a prim inside a prototype.

#### Scenario: Valid USD file is loaded

- **WHEN** `Scene::from_usd` is given a readable USD stage path
- **THEN** it returns a `Scene` with camera, world, lights, and settings populated

#### Scenario: Unreadable path

- **WHEN** the path cannot be opened as a USD stage
- **THEN** loading fails with an I/O error rather than a partial scene

### Requirement: Geometry schema mapping

The importer SHALL map `UsdGeomMesh` to kernel triangle geometry,
`UsdGeomSphere` to an analytic `Sphere`, and the implicit shapes `UsdGeomCube`,
`UsdGeomPlane`, `UsdGeomCylinder`, `UsdGeomCylinder_1`, `UsdGeomCone`,
`UsdGeomCapsule` and `UsdGeomCapsule_1` to kernel triangle geometry tessellated at
import (see "Implicit shapes are tessellated as USD tessellates them"), attaching all
of them to the single `crust-rt` scene whose BVH4 the kernel builds in `commit()`.

Mesh prims sharing identical points, topology and material binding SHALL be
treated as one *distinct mesh*. The importer SHALL choose that mesh's
representation by how many times it is placed:

- placed **exactly once** → its triangles are transformed into world space and
  attached directly, so they live in the top-level BVH;
- placed **more than once** → one shared local-space kernel scene, attached
  once per placement as an instance carrying that placement's transform.

A tessellated implicit shape SHALL be a distinct mesh like any other, decided by
the same rule.

Instancing geometry that is placed once buys no sharing while costing every
entering ray a transform into local space, a fresh traversal setup and a cold
descent into a second tree, and presents the parent BVH the transformed
bounding box of the inner tree's bounding box — a box of a box that spatial
splits cannot tighten.

Because a placement count is only final once every prim is synced, the decision
SHALL be made at the commit, over every managed prim. Geometry ids SHALL be given at
the commit in prim path order, so they depend on the stage alone, never on the
order notices arrived in.

#### Scenario: Mesh prim placed once

- **WHEN** a `UsdGeomMesh` prim is traversed and no other prim shares its
  points, topology and material
- **THEN** its triangles are transformed into world space and attached
  directly, appearing in the top-level BVH

#### Scenario: Mesh geometry placed more than once

- **WHEN** two or more prims share points, topology and material
- **THEN** one local-space kernel scene is built for that geometry and each
  prim attaches an instance of it

#### Scenario: Mirrored placement is baked

- **WHEN** a mesh placed once has a transform with negative determinant
- **THEN** the baked triangle winding is reversed, so the surface orientation
  and hence `front_face` match what the instanced path would report

#### Scenario: Left-handed mesh

- **WHEN** a mesh authors `orientation = "leftHanded"`
- **THEN** each face is read with its winding reversed, so its geometric and
  smooth normals, `front_face` and its subdivision point to the side the face
  winds clockwise from, while its faceVarying primvars and Ptex face
  coordinates read as authored

#### Scenario: Mesh prim that moves

- **WHEN** a mesh prim authors `primvars:crust:motion:translate`
- **THEN** it is attached as an instance regardless of placement count, since
  baked triangles carry no transform to interpolate over the shutter

#### Scenario: Non-invertible placement

- **WHEN** a mesh prim's transform is not invertible
- **THEN** its triangles are baked into world space, as an instance requires an
  invertible transform

#### Scenario: Sphere prim

- **WHEN** a `UsdGeomSphere` prim is traversed
- **THEN** it is added as an analytic sphere at its world-space transform, its radius
  scaled by the transform's uniform scale
- **AND** under a non-uniform scale or shear it is added as an instance of the
  authored sphere under the full transform, so it renders as an ellipsoid
- **AND** under a singular transform nothing is added

#### Scenario: Implicit shape prim

- **WHEN** a `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`, `Capsule` or
  `Capsule_1` prim is traversed, at the top level or inside a prototype
- **THEN** it renders as triangle geometry with its bound material, ray mask, motion
  and light links, exactly as a `Mesh` prim with those triangles would

#### Scenario: Identical implicit shapes share geometry

- **WHEN** two `Cylinder` prims author the same `radius`, `height`, `axis` and
  material and are tessellated to the same radial count
- **THEN** they are one distinct mesh, placed twice

### Requirement: Light schema mapping

The importer SHALL map every `UsdLux` light it reads — `SphereLight`,
`RectLight`, `DiskLight`, `CylinderLight`, `DistantLight` and `DomeLight` — onto
the lights defined by the `lighting` capability, with UsdLux units, `normalize`,
colour temperature, `ShapingAPI`, camera visibility (`crust:light:cameraVisible`,
`primvars:ri:attributes:visibility:camera`) and light linking (see "Light collection
membership") honoured as that capability states. A light's inputs SHALL be read from
its Hydra `light` data by name, `crust:light:*` included. `PortalLight`, mesh
lights and light filters SHALL NOT be read.

The importer SHALL read the render setting `domeLightCameraVisibility` (and
`crust:domeLightCameraVisibility`) from the render settings' `namespacedSettings`, as the
`lighting` capability's "Infinite lights" requirement states.

A light whose link category no receiver the import holds carries (see "Light
collection membership") illuminates nothing, as the `lighting` capability's "Lights
linked to nothing" requirement states. The test SHALL be made at the commit, over
every managed prim, so it does not depend on the order in which lights and receivers
were added.

#### Scenario: Area light

- **WHEN** a `UsdLuxRectLight`, `SphereLight`, `DiskLight` or `CylinderLight`
  prim is traversed
- **THEN** it becomes an entry in the light list and emitting geometry in the
  world

#### Scenario: Infinite light

- **WHEN** a `UsdLuxDistantLight` or `UsdLuxDomeLight` prim is traversed
- **THEN** it becomes a light-list entry with no scene geometry

#### Scenario: The Moana backdrop

- **WHEN** `island.usda` is imported, where `sky_dome_cam_llc` authors
  `collection:lightLink:excludes = </island>` and every geometry lies under
  `/island`
- **THEN** `sky_dome_cam_llc` illuminates nothing, and `sky_dome_env_llc`, whose
  excludes name only the other light, illuminates everything

#### Scenario: A light added before what it excludes

- **WHEN** a backdrop dome is added to the index, and synced, before the geometry its
  `excludes` cover
- **THEN** it still illuminates nothing once the geometry is committed

### Requirement: Adaptive subdivision level

When a target edge length is given (`float crust:subdivisionEdgeLength` on the
`RenderSettings` prim, in pixels, or the host override `--subdiv-edge-length`, which
takes precedence), the importer SHALL refine **unshared** subdivision geometry by its
size on screen, and **shared** geometry to one uniform level:

- Unshared geometry is a direct mesh prim, and a prototype placed exactly once — a
  native instancer of one instance, or the one placement of a `PointInstancer`
  prototype. Its
  meshes SHALL be tessellated per face (see "Per-face adaptive tessellation"), or,
  where that does not apply, refined to the smallest level `L` at which the mesh's
  mean cage-edge length, scaled by its world transform and by `2^-L`, projects to no
  more than the target at the nearest point of its world bounds to the render camera
  at shutter open, at the render resolution.
- Shared geometry — a prototype placed more than once — SHALL be refined to the
  resolved `crust:subdivisionLevel` / `--subdiv-level` when either is given, and to
  level 0 otherwise, whatever its distance.

Levels from the screen SHALL be clamped to `0..=max`:
- `max` is the resolved `crust:subdivisionLevel` / `--subdiv-level` when either is
  given, and 3 otherwise;
- a camera inside the bounds SHALL give `max`.

Placements SHALL be counted over the whole stage, from the instancers' topology at
the commit, so whether geometry is shared, and every level, depends only on the
stage, the camera, the resolution and the settings, never on the order prims were
added.

Without a target edge length, the uniform level SHALL apply unchanged. With a target
but no camera resolvable before traversal (from `--camera` or `RenderSettings.camera`),
the importer SHALL warn once and apply the uniform level.

A target edge length that is not a positive finite number SHALL be ignored with a
warning.

#### Scenario: Near and far copies of one cage

- **WHEN** two prims reference the same `catmullClark` cage, one filling the frame and
  one far enough that its edges project below the target at level 0, and
  `crust:subdivisionEdgeLength = 2` is authored
- **THEN** the near one is refined, and the far one renders at its cage resolution

#### Scenario: A shared prototype gets the uniform level

- **WHEN** a `PointInstancer` places one subdivision prototype both near the camera
  and far from it, in adaptive mode, with no level setting
- **THEN** every placement renders the same level-0 version, and one copy of it is
  kept

#### Scenario: A prototype placed once is adaptive

- **WHEN** an `instanceable` prim is the only placement of its prototype on the
  stage, in adaptive mode
- **THEN** its meshes are refined by their size on screen, as a direct mesh at the same
  transform would be

#### Scenario: The level setting caps adaptive refinement and sets the shared level

- **WHEN** `--subdiv-level 2` is given in adaptive mode
- **THEN** no unshared mesh is refined past level 2 (a rate of 4 per edge), and every
  shared prototype is refined to level 2

#### Scenario: No camera known before traversal

- **WHEN** a target edge length is given but neither `--camera` nor
  `RenderSettings.camera` names a camera
- **THEN** a single warning is logged and every subdivision mesh is refined to the
  uniform level

#### Scenario: Deterministic across updates

- **WHEN** an adaptive scene is committed by a delegate a host keeps, a light is added and
  removed again in place, and the same stage is imported afresh
- **THEN** every mesh placement is refined the same way in both

### Requirement: Unread geometry types are reported

A prim whose Hydra type is a geometry type the importer reads as neither a surface
nor a volume SHALL raise the `geom.unsupported_type` warning instead of being dropped
silently. It SHALL be raised once per prim type per load, carrying a count of the
prims of that type in the index: a prim inside a prototype counts once, not per
placement.

#### Scenario: Points prim

- **WHEN** a stage contains three `Points` prims and one `NurbsPatch`
- **THEN** the load's warnings carry `geom.unsupported_type` once for `Points`
  (count 3) and once for `NurbsPatch` (count 1), and nothing is rendered for them

#### Scenario: Inside a prototype

- **WHEN** an unread `Gprim` type is inside a prototype
- **THEN** it is reported in the same way

#### Scenario: Placements do not multiply the count

- **WHEN** a prototype holding one `Points` prim is placed fifty times
- **THEN** the warning for `Points` carries count 1

#### Scenario: Non-geometry prims are not reported

- **WHEN** a stage contains `Xform`, `Scope`, `Camera`, `Material` and UsdLux prims
- **THEN** no `geom.unsupported_type` warning is raised for them

### Requirement: Volume region import

Any prim carrying the constant primvar `primvars:crust:volume:type` SHALL import as a
free-standing `VolumeRegion` rather than geometry. This SHALL be checked before the
mesh, sphere, implicit-shape and light dispatch, so the region's bounds never
occlude shadow rays. A prim that Hydra instances (one carrying `instancedBy`) SHALL
be checked the same way: a volume prim inside a prototype is skipped with its
warning, and never imported as a surface. The region's box SHALL come from the
prim's `size` (when it is a `cube`) or from the unit cube, oriented and scaled by
the prim's Hydra `xform`. Its density (`homogeneous`, `smoke` procedural fBm noise,
or `grid` inline voxel data) and its σₛ, σₐ, anisotropy and emission SHALL be read
from its `primvars:crust:volume:*` primvars.

#### Scenario: Volume prim

- **WHEN** a prim authors `primvars:crust:volume:type`
- **THEN** it is added to the scene's volume regions, not to world geometry

#### Scenario: Volume Cube is not a surface

- **WHEN** a `Cube` prim authors `primvars:crust:volume:type`
- **THEN** it is a volume region only, and no box geometry is attached for it

#### Scenario: Volume Cube inside a prototype

- **WHEN** a `Cube` prim authoring `primvars:crust:volume:type` is inside a
  native-instance or `PointInstancer` prototype
- **THEN** a warning says volumes cannot be instanced, and neither a volume region
  nor box geometry is imported for it

#### Scenario: Missing grid data

- **WHEN** a `grid`-type volume's `primvars:crust:volume:gridData` length does not
  match `primvars:crust:volume:gridDims`' `nx·ny·nz`
- **THEN** a warning is emitted and the volume is skipped

### Requirement: Native instance nesting

A native instance inside another native instance's prototype SHALL be imported
through Hydra's layout: its instancer, at
`<prototype>/UsdNiPropagatedPrototypes/<hash>/__Prototype_N/UsdNiInstancer`, is
itself `instancedBy` the outer instancer. Its prototype's geometry SHALL then appear
in every placement of the outer prototype, under three composed transforms: the
outer placement, the inner instance transform and the inner prototype's transforms.
Each prototype's geometry SHALL be built once, whatever the number of placements.
Nesting deeper than the importer's instance-nesting limit SHALL be refused with a
warning. A point instancer target that lies inside a native instance SHALL place
nothing, as in UsdImaging 2.0.

#### Scenario: Instance inside a prototype

- **WHEN** a prototype `_Outer` holds a sphere at its origin and an `instanceable` prim
  referencing `_Inner` translated by `(3, 0, 0)`, and `_Inner` holds a sphere at its
  origin, and one prim instances `_Outer`
- **THEN** both spheres render: one at the instance's origin and one 3 units along X

#### Scenario: Outer prototype placed several times

- **WHEN** the outer prototype is instanced by two prims at different translations
- **THEN** each placement carries both spheres, and the inner prototype's geometry is
  built once and shared

#### Scenario: Invisible inner instance

- **WHEN** the nested `instanceable` prim is `invisible`
- **THEN** none of its geometry renders, in any placement of the outer prototype

#### Scenario: A point instancer targeting an instance's insides

- **WHEN** a `PointInstancer`'s prototype is a prim inside an `instanceable` prim
- **THEN** the instancer places nothing for that prototype, and the import does not
  fail

### Requirement: Light collection membership

Light linking SHALL be Hydra's: the light linking renderer filter evaluates each
light's `collection:lightLink` and `collection:shadowLink` (and a light filter's
`filterLink`) as `UsdCollectionAPI` membership expressions, judged with Hydra's
path expression predicates. Equal expressions share one category id. A light or
filter whose collection is trivial (everything) has no id.

The importer SHALL read, for each light, `light/lightLink` and `light/shadowLink`,
and, for each receiver:
- `categories` on geometry no instancer instances;
- `instanceCategories` per instance on a native instancer;
- `categories` on a point instancer, which apply to every instance.

A receiver SHALL be lit (or shadow-tested) by a light when the light's id is empty
or among its categories. Expressions that name a prim inside a prototype match no
instance, as in Hydra. Crust SHALL NOT judge membership itself.

#### Scenario: The nearest path decides

- **WHEN** a collection includes `/World` and excludes `/World/Set`, and
  `/World/Set/Chair` is a mesh
- **THEN** `/World/Set/Chair` does not carry the light's category and is not lit by
  it

#### Scenario: Default collection

- **WHEN** a light authors no `collection:lightLink` properties
- **THEN** every geometry is lit by it and no link data is built

#### Scenario: Explicit only

- **WHEN** a collection sets `expansionRule = "explicitOnly"` and includes
  `/World/Hero`
- **THEN** `/World/Hero/Body` is not lit by the light

#### Scenario: Per-instance links

- **WHEN** a light's `collection:lightLink` includes one of two native instances of
  one prototype
- **THEN** only that instance's geometry is lit by it, through its instancer's
  `instanceCategories`
