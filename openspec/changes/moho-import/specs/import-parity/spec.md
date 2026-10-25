## Purpose

The check that the index import route produces what the direct route produces: what
"equal" means, where it runs (tests on the committed samples and import fixtures, and a
script for ALab and Moana with memory and time bounds), and how it gates each step of
the migration and the change of default.

## ADDED Requirements

### Requirement: What equal means

Two imports of one stage, with the same options, SHALL be equal when all of these hold:
- their scene digests match. The digest covers the geometry in id order (primitives,
  transforms, materials, masks), the light list, the camera, the render settings, the
  AOV request, the warnings in order and the provenance;
- their `crust check --json` reports match, timings excluded;
- their renders at `-s 16 --indirect-clamp 0` are bitwise equal under `crust diff`.

No tolerance SHALL apply. A difference SHALL be either fixed or listed as a parity
exception with its reason. The only exception allowed SHALL be the prim paths that
warnings and provenance name inside a native prototype, until the scene index exposes
those prims' source paths. That exception SHALL be normalised by the check, never
ignored wholesale.

#### Scenario: An equal scene

- **WHEN** `samples/cornellbox.usda` is imported on both routes
- **THEN** the digests, the reports and the 16 spp images are equal

#### Scenario: A reordered geometry is caught

- **WHEN** a change makes the index route visit two meshes in the other order
- **THEN** the digests differ and the check names the first geometry id that differs

### Requirement: Parity tests on the samples and import fixtures

`cargo test` SHALL run the parity check on every committed `samples/*.usd*` scene that
imports without external assets. It SHALL also run every import test fixture in
`crust-core/tests` under both routes. A sample that cannot run in CI SHALL be listed
with its reason. The check SHALL compare digests and reports in `cargo test`. The
bitwise image check SHALL run on the samples `scripts/check_images.sh` covers.

#### Scenario: The test suite

- **WHEN** `cargo test -p crust-core --test import_parity` runs
- **THEN** every listed sample is imported on both routes and compared, and any
  difference fails the test, naming the sample and what differs

#### Scenario: Fixtures run on both routes

- **WHEN** the existing import tests run
- **THEN** each scene they build is also imported with the index route and compared
  with the direct route

### Requirement: Large-scene parity, with memory and time reported

`scripts/import_parity.sh <scene> [render flags]` SHALL import and render the scene on
both routes, interleaved as `bench_ab.sh` interleaves A and B. It SHALL compare the
`crust check` reports and the 16 spp images, and SHALL exit non-zero when they differ.
It SHALL report each route's `--stats` peak memory and import time (`Parse USD stage`)
and their ratios, without gating on them: as UsdImaging did when it moved onto scene
indices (`USDIMAGINGGL_ENGINE_ENABLE_SCENE_INDEX`, with the legacy route's tests kept
running against the same baselines), parity is the gate and performance is measured.

The results for ALab and Moana, and Moho's `moho-spike` strategy (B) peak on the same
machine as the streaming reference, SHALL be recorded in the `usd-scene-import` design
record.

#### Scenario: Moana

- **WHEN** `scripts/import_parity.sh ~/Workspace/samples/island/usd/island.usda --camera /island/cam/shotCam` runs
- **THEN** it exits 0 only if the reports and images match, and prints both routes'
  peak memory and import time with their ratios

#### Scenario: Memory and time do not gate

- **WHEN** the index route's peak memory or import time is higher than the direct
  route's, and the reports and images match
- **THEN** the script prints the ratios and exits 0

### Requirement: Parity gates every step

A schema family SHALL move to the index route only with the parity tests green. The
default route SHALL change to `index` only when the parity tests are green and
`scripts/import_parity.sh` passes on ALab and Moana. The direct route SHALL remain until
a later change removes it.

#### Scenario: A family moves

- **WHEN** the mesh family's index reader replaces its bridged reader
- **THEN** the change is accepted only with `import_parity` passing, unchanged from
  before the move

#### Scenario: The default flips

- **WHEN** the default changes to `index`
- **THEN** the recorded ALab and Moana runs for that commit pass, and
  `CRUST_USD_IMPORT=direct` still selects the direct route
