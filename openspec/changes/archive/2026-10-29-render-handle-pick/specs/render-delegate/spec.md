## MODIFIED Requirements

### Requirement: The host drives and polls the render

The delegate SHALL render on its own threads, and expose to the host:
- starting a progressive render of the committed scene, at a sample count and region;
- a restart after each update that changed the scene: the running render is
  cancelled and joined, and a fresh one started;
- `cancel`, which stops and joins the render, and `cancel_render`, which signals it to
  stop from any thread without waiting;
- `render`, the id of the render last started;
- `generation`, which moves when a newer image is published;
- `snapshot`, which returns the latest beauty, its samples per pixel, the render id,
  the time, and whether it is the completed render's final image;
- `converged` and the render's outcome;
- `aov`, which returns a named AOV once the render completed;
- `pick`, which names the prim and material under a pixel of the current render,
  from any thread; a pick during an update reports that it cannot yet.

In a build that unwinds on panic (the host's choice of profile), a panic in the
delegate or a render SHALL be returned to the host as an error, never unwind into
it. The CLI's release profile aborts on panic, as before. The decoded assets (images, texture tiles, Ptex) SHALL be kept across
updates, and an asset whose file changed on disk SHALL be decoded again.

#### Scenario: Restart on update

- **WHEN** a render is refining and the host updates with a light edit
- **THEN** the running render is cancelled, `render` moves to a new id, and the next
  snapshot names the new render

#### Scenario: Cancel without waiting

- **WHEN** an update is being committed and the host calls `cancel_render`
- **THEN** the call returns at once

#### Scenario: Agreeing with the CLI

- **WHEN** a delegate renders a stage at 16 spp to completion, and the user runs
  `crust render -i <stage> -s 16`
- **THEN** `crust diff` reports the two images identical

#### Scenario: A pick

- **WHEN** the host picks a pixel of the current render that shows a mesh
- **THEN** the pick names that mesh's prim and its bound material

#### Scenario: A pick from another thread

- **WHEN** the host's delegate stays on its index's thread and another thread picks a
  pixel of the current render that shows a mesh
- **THEN** the pick names that mesh's prim, and the next update still applies in place
