## MODIFIED Requirements

### Requirement: Pass collections use UsdCollectionAPI membership

`matte` and `cameraVisibility` SHALL be read from the active pass prim's Hydra
`collections/<name>/membershipExpression`. That is the collection's complete
`UsdCollectionAPI` membership, as UsdImaging's collection adapter computes it: nearest
include or exclude wins, `expansionRule`, included collections, and
`membershipExpression`. It SHALL be judged over the scene index with Hydra's path
expression predicates. Membership SHALL be judged on the path of the prim that brings
the geometry in. For native and PointInstancer instances that is the instancer's
instance location (the instance prim) or the point instancer prim. A path that names a
prim inside a prototype SHALL match no instance and SHALL warn once per collection.

#### Scenario: A membership expression

- **WHEN** `collection:matte` sets `includeRoot = 0` and
  `membershipExpression = "/World/Chars//"`
- **THEN** every geometry under `/World/Chars` is a holdout and nothing else is

#### Scenario: The nearest path decides

- **WHEN** `matte` includes `/World/Props` and excludes `/World/Props/Hero`
- **THEN** `/World/Props/Hero/Body` is not a holdout and `/World/Props/Crate` is

#### Scenario: An included collection anywhere on the stage

- **WHEN** a pass's `matte` includes `</World/Sets.collection:hero>`, and the hero's
  geometry is under `/World/Chars`
- **THEN** the hero is a holdout

#### Scenario: A target inside an instance

- **WHEN** `matte` includes a prim under a native instance's prototype
- **THEN** one warning names the collection, and membership is decided on the
  instance prim

### Requirement: Known gaps

The following SHALL be documented as not supported:

- `prune` and `renderVisibility` are not read, and volume regions are neither
  held out nor hidden by a pass.
- A holdout behind a thin wall that the camera ray passes straight through
  (glass at IOR 1) renders black with the wall's alpha of 1, not as a hole.
- There is no shadow catcher: a holdout records no shadow or reflection of the
  other objects into alpha.
- One render renders one pass; several passes re-import the stage.

#### Scenario: Gaps are documented

- **WHEN** a user reads the render-passes page of the user documentation
- **THEN** each gap above is listed there

### Requirement: The application names the active pass

A render SHALL have an active pass only when the host names a `RenderPass` prim
(the CLI's `--pass`, a kept delegate's `UsdImportOptions::pass`). A stage that defines
`RenderPass` prims but is rendered with none named SHALL render with no active
pass. A named path that is not a `RenderPass` prim on the stage SHALL stop the
render with an error naming the path.

#### Scenario: A named pass is active

- **WHEN** a stage defines `/Render/Passes/char` and `/Render/Passes/bg`, and the
  user runs `crust render -i shot.usda --pass /Render/Passes/char`
- **THEN** the render uses `/Render/Passes/char`'s collections and ignores
  `/Render/Passes/bg`

#### Scenario: Passes on the stage but none named

- **WHEN** the same stage is rendered with no `--pass`
- **THEN** no geometry is a holdout and no geometry is hidden from the camera by
  either pass

#### Scenario: A path that is not a pass

- **WHEN** `--pass /World/Chars` names an `Xform`
- **THEN** the render stops before importing geometry, with an error naming
  `/World/Chars` as not a `RenderPass`
