## Context

Shadow linking (lighting design record, "Shadow linking") gives a restricted light a
*link twin*. At a continuous vertex, the bounce side of that light is a shadow ray along
the bounce direction carrying the light's shadow mask. The ordinary bounce ray, stopped
by occluders the light ignores, collects nothing from it. Domes were exempt and sampled
by NEE alone: a dome covers every direction, so its twin is a shadow ray on every bounce
(+84% shadow rays on ALab), and ALab's restricted dome showed no measurable noise.

## Goals / Non-Goals

**Goals:** a restricted dome estimated without practical bias on near-specular
receivers; both halves of the NEE ↔ bounce pair consistent.

**Non-Goals:** the cheaper twin (only when the bounce escapes or stops on geometry the
dome ignores); light-link changes.

## Decisions

- **Twin the dome; do not special-case delta lobes.** After a delta bounce the escape
  already counts at full weight (`escaped_split`: no competing strategy, so
  `bounce_skips` is never asked). The bias came from continuous near-specular lobes. The
  fix is the existing MIS-combined twin, which `link_twin` already supports for lights at
  infinity (it skips them on a depth-exhausted bounce, as the ordinary escape does).
- **The pair.**
  - NEE weighs a twinned light with `light_weight(light_pdf, bounce_pdf)`, already so
    for every non-NEE-only light.
  - The twin weighs the dome with `bounce_weight(bounce_pdf, light_pdf)`, where
    `light_pdf` is the density `Light::escaped` reports, the one the escaping ray uses.
    A unit test pins that `found_along` returns exactly `escaped`'s radiance and density.
  - The ordinary bounce skips a twinned light (`bounce_skips`), unchanged.
- **`CRUST_LINK_TWIN`** already exists (a `Config` field, a row in `docs/architecture.md`).
  Its `0` is now the old estimator exactly, domes included. No new switch.

## Evidence

**The small stage.** The Moana water material on a quad, a textured dome (Moana's
`islandsun.exr`, exposure 0.3) and a 1×1 rect, without and with a dome `shadowLink`
excluding only the rect. `--variance-threshold 0 --indirect-clamp 0`, beauty mean |Δ|
between the two:

| spp | main (NEE-only dome) | this change |
|---|---|---|
| 256 | 0.0125 | 0 (bit-identical) |
| 1 024 | 0.0113 | 0 |
| 4 096 | 0.0112 | 0 |

With the twin the restricted dome's estimate is the unlinked one's on this stage, bit
for bit. The twin adds the escaped radiance at the same MIS weight the ordinary escape
would, and the rect it ignores is never in the way.

**The tests.**
- `a_restricted_dome_over_a_smooth_floor_is_unbiased`: a roughness-0 floor, a dome whose
  link excludes only a sphere hidden below the floor. Restricted and unrestricted agree
  within 5σ over 4 seeds. Under `CRUST_LINK_TWIN=0` the restricted dome reads 0.0474
  against 0.0723, 34% low.
- `a_restricted_dome_is_twinned`: light, bsdf and power agree on the glossy floor
  (depth 8, so no path ends on the depth cap, whose bounce collects no light at infinity
  under any strategy).

**Cost** (release, the same machine, one run each):

| scene | spp | render, NEE-only dome | render, twinned | shadow rays, NEE-only | shadow rays, twinned |
|---|---|---|---|---|---|
| small stage | 1 024 | 0.74 s | 1.18 s | 3.11 M | 6.88 M (+121%) |

Moana's ocean is measured on the delegate branch, where the island's dome is restricted
(main judges the emitters by path and leaves it unrestricted): see
`openspec/changes/moho-hydra-parity/design.md`.

## Risks / Trade-offs

- **[Cost]** Every scatter in a scene with a restricted dome pays a shadow ray. Accepted
  for correctness. The cheaper twin is the follow-up if it matters.
