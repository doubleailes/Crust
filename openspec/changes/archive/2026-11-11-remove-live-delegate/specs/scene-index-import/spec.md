## REMOVED Requirements

### Requirement: Every host imports through Moho

**Reason**: Replaced by "Every host commits through Moho" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

## ADDED Requirements

### Requirement: Every host commits through Moho

`crust render`, `crust check`, `crust diagnostic`, `crust light-paths` and `crust ls`
SHALL be the batch host. Each opens one stage, builds Moho's chain over it, and has
one delegate commit the scene once. Another host that hands Crust an index (see
`render-delegate`) SHALL use the same delegate code. There SHALL be no Hydra render
plugin in this repository, and no delegate kept across a host's edits: every commit
reads the whole index afresh.

#### Scenario: check reports the scene read through the delegate

- **WHEN** `crust check -i samples/cornellbox.usda --json -` runs
- **THEN** its report lists the scene the delegate committed: the same geometry,
  lights, camera and settings that `crust render` renders

