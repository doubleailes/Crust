## Purpose

How Crust reads a USD stage through the Moho scene index rather than through openusd
directly: which import route runs, which hosts follow it, how a large stage streams by
chunk, and that the index route reads exactly what the direct route reads.

## ADDED Requirements

### Requirement: Two import routes, chosen per import

The importer SHALL offer two routes over the same stage. The `direct` route reads
openusd prims. The `index` route reads a Moho scene index of the stage. The route SHALL
come from `CRUST_USD_IMPORT` (`direct` or `index`). A host SHALL be able to override it
for a single import through its import options. Any other value of `CRUST_USD_IMPORT`
SHALL be reported once as a warning and the default used.

Until the parity gate of the `import-parity` capability has passed on the samples, ALab
and Moana, the default SHALL be `direct`. After that, the default SHALL be `index`, and
`direct` SHALL remain selectable.

*Implementation status: not yet implemented. The default flips in the change's final
task group.*

#### Scenario: Default before the flip

- **WHEN** `crust render -i samples/cornellbox.usda` runs with `CRUST_USD_IMPORT` unset,
  before the flip
- **THEN** the scene is imported by the direct route, exactly as before this change

#### Scenario: Choosing the index route

- **WHEN** the same command runs with `CRUST_USD_IMPORT=index`
- **THEN** the scene is imported by the index route, and the EXR is bitwise equal to the
  direct route's at 16 spp

#### Scenario: An unknown route

- **WHEN** `CRUST_USD_IMPORT=hydra` is set
- **THEN** one warning names the unknown value and the default route imports the scene

#### Scenario: A per-import override

- **WHEN** a host imports two scenes in one process, one with the route override set to
  `index` and one without
- **THEN** the first uses the index route and the second the environment's route,
  whatever order they run in

### Requirement: Every host follows the route

`crust render`, `crust check`, `crust diagnostic`, the `crust mcp` session and the
Hydra delegate (`crust-hydra`) SHALL import through the selected route. An in-place
update in a live session SHALL rebuild through the same route as the import it updates.
An in-place update SHALL remain equal to a full re-import on both routes.

#### Scenario: check reports the index route's scene

- **WHEN** `crust check -i scene.usda --json -` runs with `CRUST_USD_IMPORT=index`
- **THEN** its report, timings aside, equals the report with `CRUST_USD_IMPORT=direct`

#### Scenario: An in-place edit on the index route

- **WHEN** an MCP session imported with the index route changes a material's input, and
  the edit is applied in place
- **THEN** the rendered image is bitwise equal to a full re-import of the edited layer
  on the index route

### Requirement: The index route reads what the direct route reads

For every prim the direct route imports, the index route SHALL import the same thing:
- the same geometry, materials, lights, volumes and cameras;
- the same render settings, products, vars and passes;
- the same warnings, with the same codes, in the same order;
- the same prims pruned (abstract, inactive, `proxy`/`guide` purpose, invisible), with
  invisible subtrees still searched for cameras.

Renderer attributes (`crust:*`) SHALL be read from the scene index's generic attribute
data, with the same authored-versus-fallback rule as the direct route.

#### Scenario: A renderer attribute

- **WHEN** a mesh authors `crust:rayMask` and `crust:subdivisionLevel`
- **THEN** the index route applies both to the mesh exactly as the direct route does

#### Scenario: Pruning and cameras

- **WHEN** a stage holds an invisible rig scope containing a camera and a light, and a
  `GEO_PROXY` scope with `purpose = "proxy"`
- **THEN** on the index route the light and the proxies are not imported, and
  `--camera` can still name the rig's camera, as on the direct route

#### Scenario: Warnings

- **WHEN** a stage raises warnings on the direct route
- **THEN** the index route raises the same warning codes, at the same prims, in the same
  order

### Requirement: The index route streams within the direct route's memory bound

When the direct route would stream (at least 4 top-level subtrees, and
`CRUST_STREAM_IMPORT` not `0`), the index route SHALL also stream. It SHALL hold at most
one chunk's payloads composed at a time, beside the payload-free scene and any materials
bound from outside the chunk. Each chunk's stage SHALL be dropped before the next is
opened. A material bound from outside a chunk SHALL resolve as it does on the direct
route.

#### Scenario: Moana

- **WHEN** the Moana island is imported on the index route
- **THEN** at most one chunk's stage is open at any time, and each is dropped before the
  next is opened

#### Scenario: A material in a sibling chunk

- **WHEN** a streamed stage binds geometry in `/World/Box` to a material under
  `/World/Looks`
- **THEN** the index route resolves that material, as the direct route does, instead of
  falling back to the default material

#### Scenario: Streaming off

- **WHEN** `CRUST_STREAM_IMPORT=0` is set
- **THEN** the index route reads one fully loaded stage, and its result equals the
  streamed index route's

### Requirement: The index owns the time

The index route SHALL resolve every value at the import's time code (`-f`), and at the
shutter offsets around it for motion, through the scene index's time. With no time
code, it SHALL read default values, as the direct route does.

#### Scenario: A frame

- **WHEN** `crust render -i samples/animation.usda -f 12 -s 16` runs on both routes
- **THEN** the two EXRs are bitwise equal

#### Scenario: Motion over the shutter

- **WHEN** `samples/motionblur.usda` is rendered at 16 spp on both routes
- **THEN** the two EXRs are bitwise equal, the moving geometry is sampled at the same
  shutter times, and the same "time-sampled motion not produced" warnings are raised
