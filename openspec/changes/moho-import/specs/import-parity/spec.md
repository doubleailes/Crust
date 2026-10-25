## Purpose

The check that the import through Moho produces what Crust's direct importer produced:
the baselines recorded from the direct importer before anything migrated, what "equal"
means, where the check runs (tests on the committed samples and import fixtures, and a
script for ALab and Moana with memory and time reported), and how it gates each step of
the migration and the direct importer's removal. As UsdImaging did when it moved onto
scene indices: the same baselines, a new route.

## ADDED Requirements

### Requirement: Baselines from the direct importer

Before any schema family migrates, baselines SHALL be recorded from the direct importer
and kept for the whole migration:
- the scene digest, for every committed sample and every import test fixture;
- the `crust check --json` report, timings excluded, for every committed sample;
- the 16 spp render at `--indirect-clamp 0`, for every committed sample and every import
  test fixture, by a hash of its pixels;
- for ALab and Moana, the `crust check` report, the 16 spp EXR, and the `--stats` peak
  memory and import time (`Parse USD stage`) as a report.

The digest SHALL cover the geometry in id order (primitives, transforms, materials,
masks), the light list, the camera, the render settings, the AOV request, the warnings
in order, the provenance, the layers and the scene's `Source`. A regenerate flag
(`CRUST_RECORD_BASELINES=1` for the tests, `record` for the script) SHALL rewrite the
baselines; it is used to record them from the direct importer, and afterwards only for a
behavioural change that is its own decision.

#### Scenario: Recording

- **WHEN** `CRUST_RECORD_BASELINES=1 cargo test -p crust-core --test import_parity`
  runs at the commit that adds the baselines
- **THEN** every listed sample's digest, report and image hash is written from the
  direct importer

### Requirement: What equal means

An import SHALL equal its baseline when all of these hold:
- its scene digest matches the baseline's;
- its `crust check --json` report matches, timings excluded;
- its render at `-s 16 --indirect-clamp 0` is bitwise equal.

No tolerance SHALL apply, and no exception: a difference SHALL be fixed. Prim paths
that warnings and provenance name inside a native prototype SHALL be equal too, named
from the scene index's `primOrigin/scenePath`. A difference that cannot be fixed in
Crust SHALL be reported (a Moho gap goes to Moho), never accepted silently, normalised
or ignored.

#### Scenario: An equal scene

- **WHEN** `samples/cornellbox.usda` is imported through Moho
- **THEN** its digest, report and 16 spp image equal the baseline's

#### Scenario: A reordered geometry is caught

- **WHEN** a change makes the import visit two meshes in the other order
- **THEN** the digests differ and the check names the first geometry id that differs

### Requirement: Parity tests on the samples and import fixtures

`cargo test` SHALL run the parity check on every committed `samples/*.usd*` scene,
comparing digests and reports with the baselines, and SHALL compare every import an
import test performs (fixtures, CLI tests, live re-imports) with its baseline digest.
`scripts/parity_images.sh` (an optimised run with `CRUST_PARITY_IMAGES=1`) SHALL also
compare each import's 16 spp image hash; an unoptimised build renders too slowly to do
so on every run. The tests that observe the second import it makes (asset requests,
log lines, render time) SHALL be listed with that reason, and no other test SHALL fail
in that mode. A sample that cannot run in CI SHALL be listed with its reason.

#### Scenario: The test suite

- **WHEN** `cargo test -p crust-core --test import_parity` runs
- **THEN** every listed sample is imported and compared with its baseline, and any
  difference fails the test, naming the sample and what differs

#### Scenario: Fixtures

- **WHEN** the existing import tests run
- **THEN** each scene they build is compared with the baseline recorded for it from the
  direct importer

### Requirement: Large-scene parity, with memory and time reported

`scripts/import_parity.sh check <dir> <scene> [render flags]` SHALL import and render
the scene with the current build and compare the `crust check` report and the 16 spp
image with the baseline in `<dir>`, and SHALL exit non-zero when they differ. It SHALL
report the current and baseline `--stats` peak memory and import time and their ratios,
without gating on them: as UsdImaging did when it moved onto scene indices, parity is
the gate and performance is measured.

The results for ALab and Moana, and Moho's `moho-spike` strategy (B) and (D) peaks on
the same machine as the streaming reference, SHALL be recorded in the
`usd-scene-import` design record.

#### Scenario: Moana

- **WHEN** `scripts/import_parity.sh check <dir> ~/Workspace/samples/island/usd/island.usda --camera /island/cam/shotCam` runs
- **THEN** it exits 0 only if the report and image match the baseline, and prints both
  peak memories and import times with their ratios

#### Scenario: Memory and time do not gate

- **WHEN** the import's peak memory or import time is higher than the baseline's, and
  the report and image match
- **THEN** the script prints the ratios and exits 0

### Requirement: Parity gates every step

A schema family SHALL move to the scene index only with the parity tests green. The
direct importer SHALL be removed only when the parity tests are green and
`scripts/import_parity.sh check` passes on ALab and Moana.

#### Scenario: A family moves

- **WHEN** the mesh family's index reader replaces its bridged reader
- **THEN** the change is accepted only with `import_parity` passing against the
  unchanged baselines

#### Scenario: The direct importer goes

- **WHEN** the direct readers, the bridge and `EvalTimeScope` are deleted
- **THEN** the parity tests are green and the ALab and Moana runs for that commit pass
  against their baselines
