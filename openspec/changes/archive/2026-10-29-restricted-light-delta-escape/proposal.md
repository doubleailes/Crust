## Why

A dome light whose `collection:shadowLink` restricts its shadow set is sampled by NEE
alone at continuous vertices; the bounce collects none of it there. That estimator is
unbiased in principle but cannot reach a near-specular lobe in practice. A roughness-0
UsdPreviewSurface is a very sharp GGX lobe, continuous, not delta. NEE almost never
samples a direction inside it, so the surface's reflection of the dome reads too dark
at any sample count.

The Moana island shows it. Its `sky_dome_env_llc` has a shadow link that excludes only
another light. Its water is a roughness-0 surface with opacity 0.5, which crust reads as
transmission at ior 1.33. The water renders olive instead of cyan. On a small stage
(that water, a textured dome, a 1×1 rect, with and without a shadow link excluding only
the rect), the difference plateaus: mean |Δ| 0.0125, 0.0113 and 0.0112 at 256, 1 024 and
4 096 spp.

The obvious suspect, emission escaping after a delta lobe, is not it: after a delta
bounce the escape already counts at full weight. The missing estimate is the
bounce-sampled one at continuous vertices, which every other shadow-linked light already
has through its link twin.

## What Changes

- A restricted dome gets a link twin, as every other restricted light already has: at
  each continuous surface or volume vertex, a shadow ray along the bounce direction with
  the dome's shadow mask, MIS-combined with NEE by the render's strategy.
  `LightKind::found_along` answers for a dome with its `escaped` radiance and density,
  at infinity.
- `LightList::set_links` and the importer stop forcing a restricted dome to NEE alone.
- `CRUST_LINK_TWIN=0`, whose off side is the old estimator, now makes every restricted
  light NEE-only, domes included, exactly as before.
- Cost: one extra shadow ray per continuous vertex in a scene with a restricted dome.
  Scenes without one are unchanged.

## Capabilities

### Modified Capabilities

- `lighting`: "Shadow linking" drops the restricted-dome exception, and "Known gaps"
  drops the NEE-only dome.

## Impact

- `crust-core`: `light/kind.rs`, `light/list.rs`, `scene/usd_import/light_links.rs`,
  `tracer/path.rs` (comments), tests.
- Images: no sample has a restricted light, so every golden is bit-identical. Scenes
  with a shadow-linked dome change: their glossy and near-specular reflections of the
  dome are no longer too dark.
