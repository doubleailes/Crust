## ADDED Requirements

### Requirement: A scene in segments

A scene builder SHALL let its caller close a segment: the geometries attached after
that point form the next segment. Geometry ids SHALL stay global across segments.
`commit()` SHALL build one acceleration structure per segment, each from its own
geometries alone.
- A closest-hit query SHALL return the closest hit over every segment.
- An occlusion query SHALL report a hit in any segment.
- Hits at the same distance in two segments SHALL resolve to the earlier segment.

A committed scene SHALL be able to rebuild one segment from a new list of geometries,
leaving the others untouched. The rebuilt scene SHALL be structurally identical to a
fresh commit of the same segments. A scene with a single segment SHALL be bit-identical
to a scene built without segments.

#### Scenario: One segment is the old scene

- **WHEN** a scene is committed without closing any segment
- **THEN** its node layout and every query's result are identical to the kernel's
  before segments existed

#### Scenario: Rebuilding the last segment

- **WHEN** a scene of two segments has its second segment rebuilt with one geometry
  added
- **THEN** its node layouts equal those of a fresh commit of the same two segments, and
  the first segment's structure was not rebuilt

#### Scenario: The closest hit across segments

- **WHEN** a ray meets a triangle of the first segment at distance 2 and a sphere of
  the second at distance 1
- **THEN** the hit is the sphere's

### Requirement: Ray masks rewritten in a committed scene

A committed scene SHALL let its caller replace the ray mask of any geometry. A scene
whose masks were rewritten SHALL answer every query exactly as a fresh commit with
those masks does. The acceleration structure's build SHALL NOT read any ray mask, so
that a mask never moves the node layout.

#### Scenario: A rewritten mask equals a fresh commit

- **WHEN** a committed scene has one geometry's mask changed from all categories to
  camera only
- **THEN** every closest-hit and occlusion query over a fixed set of rays gives the
  results of a fresh commit with that mask

#### Scenario: Masks do not move the layout

- **WHEN** the same geometries are committed twice with different masks
- **THEN** both builds produce identical node layouts
