## MODIFIED Requirements

### Requirement: One import route, through Moho

The importer SHALL read every stage as a Hydra 2.0 render delegate reads Hydra's
terminal scene index:
- through Moho's standard chain over one stage scene index;
- with Moho's light linking and dependency forwarding renderer filters over it;
- with its notices batched by prim type;
- with one managed prim per prim it renders, made by Crust's prim factory, and a
  commit after each update that builds the scene from them.

It SHALL read only Hydra's schemas: prim types, the data under Hydra's locators,
and the dirty locators. It SHALL NOT read Moho's generic attribute data
(`__usdAttributes`, `__usdRelationships`). There SHALL be one route, with no switch
to select another.

Some facts are not imaging data:
- the layers a scene composed and its root layer stack;
- layer-spec overrides;
- a material's composition arcs, for provenance;
- a `.mtlx` reference read off layer specs;
- openusd's own errors behind a diagnostic.

These SHALL be read from the stage the Rust host opened, in one host module, each
with its reason, and never by the delegate.

#### Scenario: A render

- **WHEN** `crust render -i samples/cornellbox.usda -s 16 --indirect-clamp 0` runs
- **THEN** the scene is imported through the delegate over Moho, and the EXR is
  bitwise equal to the golden `scripts/check_images.sh` recorded for this change

#### Scenario: No route to select

- **WHEN** the importer's options and the `crust::Config` switches are listed
- **THEN** none of them selects an import route

#### Scenario: No generic data is read

- **WHEN** a mesh authors an attribute that no Hydra schema carries and no primvar
  names
- **THEN** the import does not read it, and the rendered image is the one without
  that attribute

### Requirement: Every host imports through Moho

`crust render`, `crust check`, `crust diagnostic`, `crust light-paths` and `crust ls`
SHALL be the batch host. Each opens one stage, builds Moho's chain over it, and has
one delegate commit the scene in one update. A host that keeps a delegate alive (see
`render-delegate`) SHALL use the same delegate code, updated from its stage's notices.
An update applied in place SHALL equal a fresh delegate's commit. Until
`hdcrust-scene-index`, the Hydra delegate (`crust-hydra`) SHALL import the layers
hdCrust writes through the batch host, with a full import for every change.

#### Scenario: check reports the scene read through the delegate

- **WHEN** `crust check -i samples/cornellbox.usda --json -` runs
- **THEN** its report lists the scene the delegate committed: the same geometry,
  lights, camera and settings that `crust render` renders

#### Scenario: An in-place edit

- **WHEN** a host keeping a delegate changes a material's input on its stage and
  updates, and the update is applied in place
- **THEN** the rendered image is bitwise equal to a fresh delegate's over the edited
  stage

### Requirement: The index owns the time

The import SHALL resolve every value at the index's time and, for motion, at the
shutter offsets around it, with no thread-local time. With `-f t`, the index's time
SHALL be `t`. Without a time code, the index's time SHALL be time code 0, as a new
`UsdImagingStageSceneIndex` has it, and not the stage's default values. A stage
that authors both a default and time samples SHALL then read its sample at 0.
`crust check` SHALL report each attribute that authors both a default and time
samples with the `time.default_and_samples` warning, naming the attribute, because
its value without `-f` changed with this behaviour. A render SHALL NOT look for
them. A
host's time change SHALL set the index's time, and dirty only the data that
might vary over time.

#### Scenario: A frame

- **WHEN** `crust render -i samples/animation.usda -f 12 -s 16` runs
- **THEN** every animated value is read at time code 12

#### Scenario: No frame given

- **WHEN** an attribute authors `default = 1` and `timeSamples = {0: 5, 10: 7}`, and
  `crust render` runs without `-f`
- **THEN** the import reads 5

#### Scenario: A default beside samples is reported

- **WHEN** an attribute authors `default = 1` and `timeSamples = {0: 5}`, and
  `crust check --json -` runs
- **THEN** the report carries one `time.default_and_samples` warning naming the
  attribute

#### Scenario: Motion over the shutter

- **WHEN** `samples/motionblur.usda` is rendered at 16 spp
- **THEN** the moving geometry is sampled at the shutter's offsets around the index's
  time, and the same "time-sampled motion not produced" warnings are raised as before
  this change

## ADDED Requirements

### Requirement: The import follows Hydra, with listed exceptions

What the import renders SHALL be what Hydra presents the stage as: UsdImaging's
chain over the stage, with light linking and dependency forwarding as renderer
filters, as Moho reproduces them. The import SHALL interpret each prim as Hydra's
data says:
- purpose and visibility as flattened by Hydra;
- material bindings as resolved by Hydra;
- light links from Hydra's categories;
- instancing in Hydra's layout.

The following differences from the direct importer that S2 replaced SHALL be the
listed parity exceptions, each recorded in the `usd-scene-import` design record:
- **Moana's coral.** A point instancer whose prototype targets lie inside a native
  instance renders those targets empty, as UsdImaging 2.0 does. Moana's `isCoral`
  coral prototypes render nothing.
- **Purpose.** A child's own authored `purpose` overrides an ancestor's `proxy`
  or `guide`, as Hydra's flattened purpose does.
- **`over` prims.** An `over` that no layer defines is a prim of its authored type,
  as UsdImaging's traversal makes it. Its descendants render nothing.
- **Time.** The default time is time code 0 ("The index owns the time").
- **Renderer attributes.** Renderer attributes travel as Hydra carries them ("Renderer
  attributes travel as Hydra carries them").
- **Ordering.** Geometry and lights are ordered by prim path. Images that sample by
  light index differ at noise level.
- **Collections.** A collection is judged by Hydra's path expression predicates,
  so a USD-only predicate matches nothing. A binding where a strong direct binding
  meets a weaker matching collection binding takes the direct one, as UsdImaging
  does.

#### Scenario: Moana's coral

- **WHEN** the Moana island is imported
- **THEN** the `isCoral` point instancers' prototypes inside native instances place no
  geometry, and every other element renders as before this change

#### Scenario: A child purpose under a proxy

- **WHEN** a `Scope` authors `purpose = "proxy"` and its child mesh authors
  `purpose = "render"`
- **THEN** the mesh renders

#### Scenario: An undefined camera

- **WHEN** a layer authors `over Camera "renderCam"` and no layer defines that prim
- **THEN** `--camera /renderCam` renders through it, with its authored and fallback
  values

### Requirement: Renderer attributes travel as Hydra carries them

Crust's renderer attributes SHALL be read only where Hydra carries them, so that the
same scene renders the same through Moho and through a Hydra host:
- **Geometry and volume regions.** An attribute named `crust:<name>` in the
  requirements of the `usd-scene-import`, `displacement` and `lighting` capabilities
  (`crust:rayMask`, `crust:subdivisionLevel`, `crust:subdivisionEdgeLength`,
  `crust:displacementBound`, `crust:motion:translate`, `crust:volume:*`) SHALL be
  authored on a geometry or volume prim as the constant primvar
  `primvars:crust:<name>`. It SHALL be inherited by descendants as Hydra flattens
  constant primvars. This rule does not apply to UsdLux lights (below).
- **Render settings, products and vars.** Their `crust:*` attributes SHALL be read
  from Hydra's `namespacedSettings`. A setting that names a prim (`crust:dicingCamera`,
  `crust:sampleFilters`, `crust:displayFilters`, `crust:filter`) SHALL be authored as
  a `string` (or `string[]`) attribute holding the absolute prim path, because Hydra
  carries no namespaced relationship.
- **Lights.** A UsdLux light's renderer attributes SHALL stay authored as attributes
  on the light: `crust:rayMask`, `crust:light:*` and
  `primvars:ri:attributes:visibility:camera`. They SHALL be read from the light's
  Hydra `light` data, by attribute name: UsdImaging's light data answers any name
  that is not a Hydra light token with the prim's attribute of that name
  (`LookupLightParamAttribute`), and Moho's does the same. A `primvars:crust:rayMask`
  on a light SHALL NOT be read (see the `lighting` capability).
- **Precomp filters.** A precomp sample or display filter graph SHALL be a `Material`
  whose `outputs:crust:sampleFilter` or `outputs:crust:displayFilter` names its
  terminal. It SHALL be read from Hydra's material network in the `crust` context.

A path held in such a `string` setting is not remapped by composition. `crust check`
SHALL report, with the `settings.path_across_arc` warning, each one whose prim comes
through a reference or payload (where USD would have remapped a relationship
target), naming the setting and the arc.

A `crust:*` attribute or relationship authored in the form this change replaced SHALL
NOT be read. `crust check` SHALL report each one with the `attribute.legacy_crust`
warning, naming the prim and the form to author. A render SHALL NOT walk the stage to
look for them.

#### Scenario: A ray mask

- **WHEN** a mesh authors `uniform int primvars:crust:rayMask = 1`
- **THEN** the mesh is visible to camera rays only, as `crust:rayMask = 1` was

#### Scenario: An inherited subdivision level

- **WHEN** an `Xform` authors `primvars:crust:subdivisionLevel = 2` and two
  `catmullClark` meshes below it author none
- **THEN** both meshes are refined to level 2

#### Scenario: The dicing camera

- **WHEN** the render settings author `string crust:dicingCamera = "/World/dice"`
- **THEN** tessellation is sized for `/World/dice`

#### Scenario: A path setting through a reference

- **WHEN** the render settings prim comes into the stage through a reference and
  authors `string crust:dicingCamera`, and `crust check --json -` runs
- **THEN** the report carries one `settings.path_across_arc` warning naming
  `crust:dicingCamera` and the reference

#### Scenario: A legacy attribute

- **WHEN** a mesh authors `int crust:rayMask = 1` and `crust check --json -` runs
- **THEN** the report carries one `attribute.legacy_crust` warning naming the mesh and
  `primvars:crust:rayMask`, and the mesh renders as if the attribute were absent

### Requirement: Missing data reads as its default

When Hydra gives no data at a locator Crust reads, Crust SHALL read the default.
Hydra can give none because the prim has none, or because the data is a flattened
value an instancer may lack. The defaults SHALL be:
- an absent `xform` is the identity;
- an absent `visibility` is visible;
- an absent `purpose` is `default`;
- an absent primvar is unauthored;
- an absent parameter takes the node's default.

No absent value SHALL fail an import or raise a warning on its own.

#### Scenario: A native instancer without flattened data

- **WHEN** a native instancer's prim carries no `xform` and no `visibility`
- **THEN** its instances are placed by their instance transforms alone and render
  visible

#### Scenario: A light with no transform

- **WHEN** a light's data has no `xform`
- **THEN** it is placed at the origin with the identity orientation

## REMOVED Requirements

### Requirement: The import reads what the direct importer read

**Reason**: Its contract was parity with the direct importer, recorded in baselines
that this change deletes. The import now follows Hydra. Its differences from the
direct importer are listed under "The import follows Hydra, with listed exceptions".

**Migration**: No user action is needed. The image goldens of
`scripts/check_images.sh` are the regression guard. Authored renderer attributes move
as "Renderer attributes travel as Hydra carries them" states.

### Requirement: The import streams within the direct importer's memory bound

**Reason**: Moho dropped streaming for Hydra parity (it has no chunked inputs, no
merged chain and no off-stage material resolution). Crust reads one stage, as a
Hydra delegate is handed one terminal scene index. openusd now composes Moana whole
at 7.01 GiB, where the 75 GiB that motivated streaming came from openusd's
composition at the time.

**Migration**: `CRUST_STREAM_IMPORT` is removed. Setting it has no effect.
