## REMOVED Requirements

### Requirement: Displacement is deterministic

**Reason**: Replaced by "Displacement depends on the stage alone" below, without its kept-delegate scenario: no host keeps a delegate across edits.
**Migration**: None.

## ADDED Requirements

### Requirement: Displacement depends on the stage alone

The displaced positions SHALL depend only on the stage, the camera, the resolution and the
settings, never on the order prims were added to the scene index or on the number
of threads.

#### Scenario: One thread and many

- **WHEN** a mesh is displaced on one thread and on the default thread pool
- **THEN** the displaced points, normals and largest offset are bit-identical

