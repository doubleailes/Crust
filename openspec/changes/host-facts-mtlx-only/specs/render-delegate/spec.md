## MODIFIED Requirements

### Requirement: The image depends on the scene index alone

The scene the delegate commits, and so the image it renders, SHALL depend only
on the scene index the host gave it and on the host's render settings for the
delegate (samples, region). The delegate SHALL ask its host about the stage
(`HostFacts`) for one thing only: the `.mtlx` document a material references,
which openusd does not compose (removed by `mtlx-format-plugin`).

In particular, the delegate SHALL read from the index:
- every asset path, as the index's `asset` value gives it (resolved, or
  authored); a relative path the index leaves unresolved resolves against the
  directory of the stage path the host gave;
- the stage's unit, from the absolute root's `__usdMetersPerUnit/metersPerUnit`,
  unauthored when absent.

#### Scenario: No host facts, the same image

- **WHEN** a sample that references no `.mtlx` document is rendered at 16
  spp once with `NoHostFacts` and once with the stage's `StageHostFacts`
- **THEN** the two images are bit-identical

#### Scenario: The unit from the index

- **WHEN** a stage authors `metersPerUnit = 1`, its render camera authors
  depth of field, and the delegate is updated with `NoHostFacts`
- **THEN** the lens is the one the existing depth-of-field requirement gives
  for `metersPerUnit = 1`

#### Scenario: The listed exception

- **WHEN** a material's only opinion is a reference into a `.mtlx` document
  and the delegate is updated with `NoHostFacts`
- **THEN** the material renders as an empty material, and with
  `StageHostFacts` it renders through the referenced document

#### Scenario: One question

- **WHEN** a host implements `HostFacts`
- **THEN** the only method it answers is the `.mtlx` reference of a material

## ADDED Requirements

### Requirement: The delegate's warnings come from the index

Every warning and error the delegate raises about the scene SHALL be decided
from the index, never from the host's stage. A stage's time range SHALL be the
root's `sceneGlobals/startTimeCode` and `endTimeCode`, a `0..0` range and a range
whose start lies after its end (a start authored alone, past the end's `0`
fallback) reading as unauthored. A pass that is not a `RenderPass` SHALL be explained from the index:
no prim at its path, or the type of the prim there (its USD type name as the
index's prim info gives it, else its Hydra type). Causes the index does
not carry (an `xformOp` kind the schema registry does not know, an `xformOp`
stack openusd cannot compose, an instanceable prim with no prototype) SHALL raise
no warning.

#### Scenario: A frame outside the range

- **WHEN** a stage authors `startTimeCode = 1001` and `endTimeCode = 1050` and is
  rendered at frame 900 with `NoHostFacts`
- **THEN** `time.outside_range` is raised once

#### Scenario: No range authored

- **WHEN** a stage authors no time range and is rendered at frame 900
- **THEN** no `time.outside_range` is raised

#### Scenario: A start authored alone
- **WHEN** a stage authors `startTimeCode = 1001` and no `endTimeCode`, and is rendered at frame 1010
- **THEN** no `time.outside_range` is raised

#### Scenario: A pass that is a mesh

- **WHEN** the host's pass names a `Mesh` prim
- **THEN** the import fails with the pass error, naming the path and the type `Mesh`

### Requirement: The batch host stamps the stage's layers

`crust render`, which opens the stage, SHALL write `crust:layerStack` and
`crust:invisibleOverrides` from that stage, read before the delegate commits, as
the image-output requirements state. A host that gives the delegate an index
and no such facts SHALL leave both unset.

#### Scenario: The stamp after a render

- **WHEN** `crust render` renders a stage with an override layer hiding one prim
- **THEN** the EXR's `crust:invisibleOverrides` is `1` and `crust:layerStack` lists
  the override layer and then the shot, as before this change
