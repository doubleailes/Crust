## ADDED Requirements

### Requirement: The image depends on the scene index alone

The scene the delegate commits, and so the image it renders, SHALL depend only
on the scene index the host gave it and on the host's render settings for the
delegate (samples, region). What the delegate asks its host about the stage
(`HostFacts`) SHALL only inform warnings, `crust check` reports and
provenance, with one exception: the `.mtlx` document a material references,
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
