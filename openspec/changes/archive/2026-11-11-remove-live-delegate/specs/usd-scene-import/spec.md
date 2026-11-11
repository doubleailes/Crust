## MODIFIED Requirements

### Requirement: Render settings can disable depth of field

`disableDepthOfField = true` SHALL render through a pinhole at the camera's
position, whatever its `fStop`. The field of view and the focus plane SHALL not
change. The value SHALL be resolved like `disableMotionBlur`: the first
product's authored value, else the settings prim's, default `false`. It SHALL
not be warned about as not honoured.

#### Scenario: Sharp render with a lens authored

- **WHEN** the render camera authors `fStop = 2` and the render settings author
  `disableDepthOfField = true`
- **THEN** every camera ray starts at the camera's position, and no warning names
  `disableDepthOfField`

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableDepthOfField = true` and the first
  product authors `disableDepthOfField = false`, and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Depth of field stays on by default

- **WHEN** `disableDepthOfField` is not authored and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Products that disagree

- **WHEN** the first product authors `disableDepthOfField = true` and a second
  product with the same camera and resolution authors
  `disableDepthOfField = false`
- **THEN** both products are written without depth of field and one
  `product.depth_of_field_mismatch` warning names the second product

## REMOVED Requirements

### Requirement: Render settings can disable motion blur

**Reason**: Replaced by "Render settings disable motion blur" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

### Requirement: Everything that reads the camera's framing reads the conformed one

**Reason**: Replaced by "What reads the camera's framing reads the conformed apertures" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

### Requirement: Adaptive subdivision level

**Reason**: Replaced by "Adaptive subdivision refines by size on screen" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

## ADDED Requirements

### Requirement: Adaptive subdivision refines by size on screen

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

### Requirement: Render settings disable motion blur

`disableMotionBlur = true` SHALL render moving geometry sharp where it is at the
frame (UsdRender's `[0, 0]` shutter at the current time code), in the beauty and
every AOV. `instantaneousShutter = true` SHALL also turn blur off, but sharp at
shutter open, `f + shutter:open`; with both, `disableMotionBlur` wins. Motion
stays in the scene, so motion vectors are still produced. Each value is resolved
like the render's camera: the first product's, else the settings prim's.
Neither SHALL be warned about as not honoured.

#### Scenario: Sharp beauty with motion authored

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp at its authored position, with no streak
  along X, and no warning about `disableMotionBlur` is logged

#### Scenario: Sharp beauty at the frame for an animated transform

- **WHEN** a sphere's transform is time-sampled, the camera authors
  `shutter:open = -0.25` and `shutter:close = 0.25`, the render is made with `-f 5`, and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp where its transform places it at time code 5, and its
  `motionvector` is the screen displacement its transform makes from time code 4.75 to
  5.25, measured from there

#### Scenario: instantaneousShutter is sharp at shutter open

- **WHEN** the same stage authors `instantaneousShutter = true` and not
  `disableMotionBlur`
- **THEN** the sphere renders sharp where its transform places it at time code 4.75

#### Scenario: instantaneousShutter is a synonym

- **WHEN** the camera's `shutter:open` is 0, or the stage authors no shutter, and the
  render settings author `instantaneousShutter = true` and not `disableMotionBlur`
- **THEN** the render is the same as with `disableMotionBlur = true`

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableMotionBlur = true` and the first
  product authors `disableMotionBlur = false`
- **THEN** moving geometry is motion blurred

#### Scenario: Blur stays on by default

- **WHEN** neither attribute is authored
- **THEN** a prim authoring `crust:motion:translate` is motion blurred, as
  before this change

### Requirement: What reads the camera's framing reads the conformed apertures

Adaptive subdivision and on-screen implicit-shape sizing SHALL read the same
conformed apertures as the render camera, so a policy that reframes the image also
re-sizes what is tessellated for it.

#### Scenario: Adaptive subdivision follows the policy

- **WHEN** a stage with adaptive subdivision changes only its
  `aspectRatioConformPolicy` so that the conformed vertical aperture changes
- **THEN** the tessellation is computed for the conformed field of view

