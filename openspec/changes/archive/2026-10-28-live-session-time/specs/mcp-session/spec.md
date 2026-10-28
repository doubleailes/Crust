## ADDED Requirements

### Requirement: A host session renders at a time code

A live session a host drives SHALL render the stage at the time code the host
sets, as `crust render --frame` renders it: time-sampled attributes resolve at
that time and the sampler's frame seed follows it. Without a time it SHALL
render default values. A time that is not finite SHALL be refused.

#### Scenario: No time

- **WHEN** a host never sets a time
- **THEN** the session renders default values

#### Scenario: A time that is not finite

- **WHEN** a host sets the time to NaN or infinity
- **THEN** the call is refused

### Requirement: A time change applies at the next apply

A change of time SHALL take effect at the next apply, and the render after it
SHALL be bit-identical to a fresh session's render of the same files at that
time. Edits at an unchanged time SHALL still apply in place.

#### Scenario: Changing the time

- **WHEN** a host renders an owned layer whose prim's translate is sampled at
  1 and 10 at time 1, then sets the time to 10 and applies
- **THEN** the change is applied as `full`, the image differs from the one at
  1, and it is bitwise the render of a fresh session at 10

#### Scenario: An edit at a time

- **WHEN** a host renders at time 10 and changes a light's
  `inputs:intensity` in an owned layer
- **THEN** the change is applied as `lights`, and the render is bitwise a
  fresh session's at 10

### Requirement: Images report their time

Every image the session publishes SHALL report the time code its render used,
and none when it rendered default values.

#### Scenario: After a time change

- **WHEN** a host renders at time 1, sets the time to 10 and applies
- **THEN** the images of the first render report time 1 and those of the
  second report time 10

#### Scenario: Default values

- **WHEN** a host never sets a time
- **THEN** its images report no time
