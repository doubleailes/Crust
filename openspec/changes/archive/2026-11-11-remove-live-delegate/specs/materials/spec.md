## REMOVED Requirements

### Requirement: Curve attributes resolve as per-query shading does

**Reason**: Replaced by "Curve attributes resolve as per-query shading" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

## ADDED Requirements

### Requirement: Curve attributes resolve as per-query shading

On a curve hit, a material reading curve properties or curve primvars SHALL
resolve to exactly what per-query shading of the same hit gives.

#### Scenario: Resolve matches per-query shading on a curve

- **WHEN** the resolve pin runs a curve-reading material on a curve hit
- **THEN** the resolved shading point is bit-identical to per-query shading

