## MODIFIED Requirements

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

The lights SHALL form the scene's light sources apart from its other geometry:
- the light list SHALL hold the lights in prim path order;
- an area light's emitting geometry SHALL take geometry ids after every geometry the
  "Geometry schema mapping" requirement numbers, in the light list's order;
- the emitting geometry SHALL be held in a kernel segment of its own, which the
  emitters alone build.

The light sources therefore depend on the light prims alone, and can be built again
without renumbering or rebuilding any other geometry. Hidden, linked or camera-only
emitters SHALL keep the visibility the `lighting` capability gives them.

#### Scenario: Area light

- **WHEN** a `UsdLuxRectLight`, `SphereLight`, `DiskLight` or `CylinderLight`
  prim is traversed
- **THEN** it becomes an entry in the light list and emitting geometry in the
  world

#### Scenario: Infinite light

- **WHEN** a `UsdLuxDistantLight` or `UsdLuxDomeLight` prim is traversed
- **THEN** it becomes a light-list entry with no scene geometry

#### Scenario: Emitters are numbered after the other geometry

- **WHEN** a stage holds `/A/Light` (a `RectLight`) and `/B/Mesh`
- **THEN** the mesh's geometry id is lower than the light's emitter's, though the light
  comes first in path order

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
