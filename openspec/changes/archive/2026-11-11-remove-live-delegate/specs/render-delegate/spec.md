## REMOVED Requirements

### Requirement: A host hands the delegate a scene index

**Reason**: Replaced by "A host commits the delegate from a scene index" below: a delegate is committed afresh on each call, so the host-driven edit scenario no longer applies.
**Migration**: Call `SceneDelegate::commit` where a host called `update`.

### Requirement: Updates apply in place from the dirty locators

**Reason**: Mantle, the only live host, is cancelled. No host keeps a delegate across
edits, so the update classes and the in-place rebuilds have no caller.
**Migration**: A host that edits its stage commits again; every commit reads the
whole index afresh.

### Requirement: A time change is an index time change

**Reason**: It specified how a kept delegate applies a time change in place. With no
kept delegate, a time change is a fresh commit at the index's new time, which the
`usd-scene-import` time requirements already cover.
**Migration**: Set the time on the stage scene index and commit.

### Requirement: The host drives and polls the render

**Reason**: Render control for a live host (restart on update, snapshots taken by
the host, pick) was `crust-render`'s `delegate` feature, removed with Mantle.
**Migration**: None. `crust render`'s cancellation and progress snapshots are the
`rendering` and `cli` capabilities', and are unchanged.

### Requirement: Update latency is reported, not gated

**Reason**: The `update_latency` example measured in-place updates, which no longer
exist.
**Migration**: None.

### Requirement: A settings update reads only the settings and cameras

**Reason**: An in-place update class, removed with the update classes.
**Migration**: None.

### Requirement: A re-commit holds one scene

**Reason**: It governed a kept delegate's re-commit. A delegate is now committed by
its host and holds no scene between commits.
**Migration**: None.

## ADDED Requirements

### Requirement: A host commits the delegate from a scene index

Crust's delegate SHALL take its scene from Moho's `chain::standard` over a stage
a host opened, with Moho's light linking and dependency forwarding filters added.
Each `commit` SHALL commit the whole scene afresh from the index, at the index's
time. The delegate SHALL own no stage, open no file and read no layer.

`crust render`, `crust check`, `crust diagnostic`, `crust light-paths` and `crust ls`
SHALL be the batch host: each opens the stage and commits one delegate once.

#### Scenario: The batch host

- **WHEN** `crust render -i samples/cornellbox.usda -s 16` runs
- **THEN** the scene rendered is the one the delegate committed from the chain over
  the opened stage
