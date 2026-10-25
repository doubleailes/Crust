## Purpose

How Crust reads a USD stage: through the Moho scene index, which every host imports
through. It covers how a large stage streams by chunk, the index owning the time, and
that the import through Moho reads exactly what Crust's direct importer read before it
was removed.

## ADDED Requirements

### Requirement: One import route, through Moho

The importer SHALL read every stage through a Moho scene index of it: Hydra prims,
schemas and Moho's generic attribute data, never `usd::Prim` readers walking the stage.
There SHALL be one route. The direct importer, which read openusd prims, SHALL be
removed, with no switch to select it. Facts that are not imaging data (the layers a
scene composed, layer-spec overrides, a material's composition arcs for provenance, a
`.mtlx` reference read off layer specs) SHALL be read from the indexed stage in one
module, each with its reason.

*Implementation status: in progress. The direct importer is removed in the change's
final task groups.*

#### Scenario: A render

- **WHEN** `crust render -i samples/cornellbox.usda -s 16 --indirect-clamp 0` runs
- **THEN** the scene is imported through the Moho scene index, and the EXR is bitwise
  equal to the baseline recorded from the direct importer

#### Scenario: No route to select

- **WHEN** the importer's options and the `crust::Config` switches are listed
- **THEN** none of them selects an import route

### Requirement: Every host imports through Moho

`crust render`, `crust check`, `crust diagnostic`, the `crust mcp` session and the
Hydra delegate (`crust-hydra`) SHALL import through the Moho scene index. An in-place
update in a live session SHALL rebuild through the same index readers as the import it
updates, and SHALL remain equal to a full re-import.

#### Scenario: check reports the scene read through Moho

- **WHEN** `crust check -i samples/cornellbox.usda --json -` runs
- **THEN** its report, timings aside, equals the baseline recorded from the direct
  importer

#### Scenario: An in-place edit

- **WHEN** an MCP session changes a material's input, and the edit is applied in place
- **THEN** the rendered image is bitwise equal to a full re-import of the edited layer

### Requirement: The import reads what the direct importer read

For every prim the direct importer imported, the import through Moho SHALL import the
same thing, as recorded in the baselines (`import-parity`):
- the same geometry, materials, lights, volumes and cameras;
- the same render settings, products, vars and passes;
- the same warnings, with the same codes, in the same order;
- the same prims pruned (abstract, inactive, `proxy`/`guide` purpose, invisible), with
  invisible subtrees still searched for cameras.

Renderer attributes (`crust:*`) SHALL be read from the scene index's generic attribute
data, with the same authored-versus-fallback rule as the direct importer.

#### Scenario: A renderer attribute

- **WHEN** a mesh authors `crust:rayMask` and `crust:subdivisionLevel`
- **THEN** both apply to the mesh exactly as the direct importer applied them

#### Scenario: Pruning and cameras

- **WHEN** a stage holds an invisible rig scope containing a camera and a light, and a
  `GEO_PROXY` scope with `purpose = "proxy"`
- **THEN** the light and the proxies are not imported, and `--camera` can still name
  the rig's camera, as with the direct importer

#### Scenario: Warnings

- **WHEN** a stage's baseline records warnings from the direct importer
- **THEN** the import raises the same warning codes, at the same prims, in the same
  order

### Requirement: The import streams within the direct importer's memory bound

When a stage has at least 4 top-level subtrees, and `CRUST_STREAM_IMPORT` is not `0`,
the import SHALL stream, as the direct importer did. It SHALL hold at most
one chunk's payloads composed at a time, beside the payload-free scene and any materials
bound from outside the chunk. Each chunk's stage SHALL be dropped before the next is
opened. A material bound from outside a chunk SHALL resolve as it did with the direct
importer.

#### Scenario: Moana

- **WHEN** the Moana island is imported
- **THEN** at most one chunk's stage is open at any time, and each is dropped before the
  next is opened

#### Scenario: A material in a sibling chunk

- **WHEN** a streamed stage binds geometry in `/World/Box` to a material under
  `/World/Looks`
- **THEN** the import resolves that material, as the direct importer did, instead of
  falling back to the default material

#### Scenario: Streaming off

- **WHEN** `CRUST_STREAM_IMPORT=0` is set
- **THEN** the import reads one fully loaded stage, and its result equals the streamed
  import's

### Requirement: The index owns the time

The import SHALL resolve every value at the import's time code (`-f`), and at the
shutter offsets around it for motion, through the scene index's time, with no
thread-local time. With no time code, it SHALL read default values, as the direct
importer did.

#### Scenario: A frame

- **WHEN** `crust render -i samples/animation.usda -f 12 -s 16` runs
- **THEN** the EXR is bitwise equal to the baseline recorded from the direct importer

#### Scenario: Motion over the shutter

- **WHEN** `samples/motionblur.usda` is rendered at 16 spp
- **THEN** the EXR is bitwise equal to its baseline, the moving geometry is sampled at
  the same shutter times, and the same "time-sampled motion not produced" warnings are
  raised
