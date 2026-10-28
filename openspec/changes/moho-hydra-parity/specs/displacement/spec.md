## MODIFIED Requirements

### Requirement: Displacement is deterministic

The displaced positions SHALL depend only on the stage, the camera, the resolution and the
settings, never on the order prims were added to the scene index, on an in-place update,
or on the number of threads.

#### Scenario: A live update and a fresh import

- **WHEN** a displaced scene is imported in a live session, a light is added in place,
  and the same layer is then imported afresh
- **THEN** the two renders are bit-identical
