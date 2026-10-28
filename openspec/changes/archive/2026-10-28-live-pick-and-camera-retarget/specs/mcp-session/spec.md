## MODIFIED Requirements

### Requirement: Edits apply in place when they are confined

The session SHALL compare the override layer before and after each edit batch or undo,
and apply the change without a re-import when every changed field belongs to one of
these classes:

- `settings`: attribute values on the `RenderSettings` prim the render uses, or on
  the render camera, its `xformOp:*` and `xformOpOrder` values included when no
  prim the scene imported (geometry, a volume region, a light, a material) lies below
  the camera; and the targets of the `camera` relationship on that `RenderSettings`
  prim or on a `RenderProduct`, when the camera a full import would then render
  through is one the import met, no prim the scene imported lies below it, and its
  shutter opens and closes as the old render camera's did;
- `lights`: `inputs:*` values on existing UsdLux light prims;
- `materials`: `inputs:*` values on existing shaders, or interface inputs, of
  materials the scene already uses, when the change does not move the material's
  opacity, transmission or emission class;
- `geometry`: a transform on a prim whose subtree places only meshes, static
  analytic spheres and lights, none of them moving over the shutter, with no volume
  region or camera the render reads below it; the points, normals, topology,
  subdivision tags or primvars of a mesh placed exactly once, by no instancer or
  prototype, not displaced, when the mesh does not become identical to another the
  scene places and subdivision is not adaptive; an area light's transform or shape
  inputs. A distant or dome light's transform is a `lights` edit.

Any other change SHALL be applied as `full`, a re-import of the saved layer. This
includes a spec added or removed, another relationship's targets, a connection,
visibility, variant
selection, activation or composition arc, and a transform or reshape the `geometry`
class does not cover. A batch with changes in several classes SHALL be applied as the
most expensive of them, in the order `settings` < `materials` < `lights` < `geometry`
< `full`. A render after an edit applied in place
SHALL be bit-identical to a render after a `full` application of the same layer
content. With `CRUST_SCENE_DELTAS=0`, every edit SHALL be applied as `full`, with no
assets kept from earlier imports.

Three properties of a production stage SHALL NOT, on their own, make a confined edit
`full`:

- **Tessellation that reads the camera.** When the scene uses adaptive subdivision or
  implicit shapes sized on screen, a `settings` edit SHALL stay in place unless the
  camera projection that tessellation reads changes. That projection is the dicing
  camera's (`crust:dicingCamera`, else the render camera): its position,
  orientation, focal length, aperture, or the image resolution, or the
  adaptive-subdivision target or level ceiling. Exposure, focus distance, f-stop,
  shutter, sampling settings and render products do not move it, and neither does
  moving a render camera that is not the dicing camera, or retargeting the render
  camera while another camera dices.
- **Light linking.** When the scene authors `collection:lightLink` or
  `collection:shadowLink`, a `lights` edit SHALL stay in place, and the resolved links
  SHALL be kept. An edit to a light that the links leave illuminating nothing (a light
  the scene no longer lists as a light) SHALL be `full`.
- **Looks delivered by payloads.** A `materials` edit SHALL classify the same whether
  the material's network is composed directly or through a payload.

#### Scenario: A shader input

- **WHEN** the agent sets `inputs:base_color` on an existing `standard_surface` shader
- **THEN** the result reports `applied = materials`, and a 16 spp render is
  bit-identical to the same render after `crust render <output>` imports the layer

#### Scenario: A transform is not confined

- **WHEN** the agent sets `xformOp:translate` on an ancestor of the render camera
- **THEN** the result reports `applied = full`

#### Scenario: A mesh's transform is a geometry edit

- **WHEN** the agent sets `xformOp:translate` on a mesh the scene places once
- **THEN** the result reports `applied = geometry`, and a 16 spp render is
  bit-identical to the same render after `crust render <output>` imports the layer

#### Scenario: Reshaping a shared mesh is not confined

- **WHEN** the agent sets `points` on one of two prims that place the same mesh
- **THEN** the result reports `applied = full`

#### Scenario: Making a material emissive

- **WHEN** the agent sets `inputs:emission` above 0 on a shader whose material
  emitted nothing
- **THEN** the result reports `applied = full`

#### Scenario: The switch turns it off

- **WHEN** the session runs with `CRUST_SCENE_DELTAS=0` and the agent sets
  `inputs:intensity` on a light
- **THEN** the result reports `applied = full`

#### Scenario: Exposure on an adaptively subdivided scene

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and the agent sets
  `exposure` on the render camera
- **THEN** the result reports `applied = settings`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: Focal length on an adaptively subdivided scene

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and the agent sets
  `focalLength` on the render camera
- **THEN** the result reports `applied = full`

#### Scenario: Dimming a linked light

- **WHEN** the scene's lights author `collection:lightLink` and
  `collection:shadowLink`, and the agent sets `inputs:intensity` on a linked light
- **THEN** the result reports `applied = lights`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: A light that illuminates nothing

- **WHEN** a light's `collection:lightLink` includes no receiver, and the agent sets
  its `inputs:intensity`
- **THEN** the result reports `applied = full`

#### Scenario: A shader behind a payload

- **WHEN** a mesh's material is defined in a layer the stage brings in through a
  payload, and the agent sets an `inputs:roughness` on that material's shader
- **THEN** the result reports `applied = materials`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: Moving the render camera

- **WHEN** the scene uses neither adaptive subdivision nor implicit shapes sized on
  screen, and the agent sets `xformOp:translate` on the render camera
- **THEN** the result reports `applied = settings`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: Moving the render camera past a dicing camera

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and `crust:dicingCamera` to
  a camera other than the render camera, and the agent sets `xformOp:rotateXYZ` on
  the render camera
- **THEN** the result reports `applied = settings`, the tessellation is unchanged,
  and a 16 spp render is bit-identical to the same render after a `full`
  application of the layer

#### Scenario: Moving the dicing camera

- **WHEN** the scene sets `crust:subdivisionEdgeLength`, and the agent sets
  `xformOp:translate` on the camera `crust:dicingCamera` names
- **THEN** the result reports `applied = full`

#### Scenario: A camera with prims below it

- **WHEN** a mesh is authored as a child of the render camera, and the agent sets
  `xformOp:translate` on the camera
- **THEN** the result reports `applied = full`

#### Scenario: Switching the render camera

- **WHEN** the scene uses neither adaptive subdivision nor implicit shapes sized on
  screen, and the agent sets `rel camera` on the render settings prim, or on the
  render product, to another camera of the stage
- **THEN** the result reports `applied = settings`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: Switching the render camera that dices

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and no `crust:dicingCamera`,
  and the agent retargets `rel camera` on the render settings prim to another camera
- **THEN** the result reports `applied = full`

#### Scenario: Switching to a prim that is not a camera

- **WHEN** the agent retargets `rel camera` on the render settings prim to a mesh
- **THEN** the result reports `applied = full`

#### Scenario: Another relationship

- **WHEN** the agent retargets a mesh's `material:binding`
- **THEN** the result reports `applied = full`

## ADDED Requirements

### Requirement: A host names what a pixel shows

A live session a host drives SHALL name, for a pixel of the current render's image
(as its snapshots lay it out), the prim whose geometry the pixel-centre camera ray of
that render first meets, and the material bound to it. A hit inside a native instance
or a PointInstancer SHALL name the instance or instancer prim. When the session no
longer holds the geometry that render drew, it SHALL refuse as busy rather than name
a prim of another scene.

#### Scenario: Picking a mesh

- **WHEN** a host picks a pixel on a mesh bound to a material
- **THEN** the answer names the mesh's prim and the material's

#### Scenario: Picking inside an instancer

- **WHEN** a host picks a pixel on a prototype's mesh placed by a PointInstancer, or
  inside a native instance
- **THEN** the answer names the PointInstancer prim, or the instance prim

#### Scenario: Picking the background

- **WHEN** a host picks a pixel where the ray meets no geometry
- **THEN** the answer is empty, not an error

#### Scenario: Picking after the scene was dropped

- **WHEN** a load fails after a render, so the session holds no scene, and the host
  picks a pixel of that render's image
- **THEN** the pick is refused as busy
