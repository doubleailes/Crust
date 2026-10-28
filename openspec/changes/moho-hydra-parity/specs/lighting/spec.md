## ADDED Requirements

### Requirement: A light's renderer attributes are read from its Hydra light data

The renderer attributes that the lighting requirements name on a UsdLux light SHALL
stay authored as attributes on the light prim:
- `crust:rayMask`, which decides the source geometry's visibility outright ("Area
  lights") and the escape mask of a light at infinity;
- `crust:light:cameraVisible` and `crust:light:lpeTag`;
- `primvars:ri:attributes:visibility:camera`.

They SHALL be read from the light's Hydra `light` data by attribute name. UsdImaging
answers any name that is not a Hydra light token with the prim's attribute of that
name (`LookupLightParamAttribute`), so they reach Crust through Moho and through a
Hydra host alike. The constant-primvar form that geometry uses for `crust:rayMask`
(`primvars:crust:rayMask`) SHALL NOT be read on a light. A light SHALL NOT inherit a
`primvars:crust:*` value from an ancestor. The precedence is unchanged: an authored
`crust:rayMask`, then `crust:light:cameraVisible`, then
`primvars:ri:attributes:visibility:camera`.

#### Scenario: A visible rect light through Hydra

- **WHEN** a `RectLight` authors `int crust:rayMask = 1`
- **THEN** its source geometry is visible to camera rays, as before this change

#### Scenario: The geometry form on a light is not read

- **WHEN** a `SphereLight` authors `primvars:crust:rayMask = 1` and nothing else that
  decides its visibility
- **THEN** its source geometry stays invisible to camera rays, the default

#### Scenario: An Xform's primvar does not reach its light

- **WHEN** an `Xform` authors `primvars:crust:rayMask = 0` and holds a mesh and a
  `DiskLight` with `crust:light:cameraVisible = 1`
- **THEN** the mesh takes ray mask 0 and the light's source geometry is visible to
  camera rays
