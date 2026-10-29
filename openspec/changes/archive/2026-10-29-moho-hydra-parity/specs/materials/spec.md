## MODIFIED Requirements

### Requirement: Curve attributes resolve as per-query shading does

On a curve hit, a material reading curve properties or curve primvars SHALL
resolve to exactly what per-query shading of the same hit gives. A kept delegate's
update that makes a curve material read a property or primvar it did not read
before SHALL render as a fresh commit of the edited stage does.

#### Scenario: Resolve matches per-query shading on a curve

- **WHEN** the resolve pin runs a curve-reading material on a curve hit
- **THEN** the resolved shading point is bit-identical to per-query shading

#### Scenario: A live edit starts reading a primvar

- **WHEN** a host edits a kept delegate's curve material to read
  `geompropvalue` `hairColor` it did not read before, and updates
- **THEN** the next render is bit-identical to a fresh delegate's over the edited
  stage
