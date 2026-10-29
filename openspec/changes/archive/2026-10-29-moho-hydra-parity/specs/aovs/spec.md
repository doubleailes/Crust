## MODIFIED Requirements

### Requirement: Primvar sources read the hit prim only

*Not yet supported: per-instance primvars.*

A primvar var SHALL read the primvar Hydra gives the geometry prim that was hit: a
mesh, or a `BasisCurves` / `NurbsCurves` prim. A constant primvar authored on an
ancestor prim SHALL be inherited, as Hydra's flattening inherits constant primvars
(see `scene-index-import`, "Renderer attributes travel as Hydra carries them"). A
per-instance primvar of a `PointInstancer`, and a primvar authored on an instance prim,
SHALL NOT be inherited. Such a primvar SHALL be reported with one warning per var, and
the prims below it SHALL read the clear value.

#### Scenario: An ancestor's constant primvar is inherited

- **WHEN** a var asks for `objectId`, and an `Xform` authors
  `constant int primvars:objectId = 3` above a mesh that authors none
- **THEN** no warning is raised, and the mesh reads 3

#### Scenario: A point instancer's per-instance primvar is reported

- **WHEN** a var asks for `objectId`, and a `PointInstancer` authors
  `int[] primvars:objectId` per instance
- **THEN** a warning names the `PointInstancer`, and its instances read the clear value

#### Scenario: Curves primvar

- **WHEN** a var asks for `displayColor` on a `BasisCurves` prim authoring a
  `uniform` `primvars:displayColor`
- **THEN** each strand's pixels hold its curve's colour
