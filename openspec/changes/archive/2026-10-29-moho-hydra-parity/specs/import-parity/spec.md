## REMOVED Requirements

### Requirement: Baselines from the direct importer

**Reason**: The direct importer is gone and the import now follows Hydra (see
`scene-index-import`, "The import follows Hydra, with listed exceptions"). Baselines
recorded from it would hold the import to behaviour Hydra does not have (Moana's coral,
the default time, path-ordered ids). The 3 603 baseline files under
`crates/crust-core/tests/data/import_baselines/` are deleted with the capability.

**Migration**: The image goldens of `scripts/check_images.sh` (record before a change,
check after it) are the regression guard. A difference from the direct importer is
listed in the `usd-scene-import` design record.

### Requirement: What equal means

**Reason**: There is no baseline to compare an import with. Equality between an
in-place update and a full re-import stays, in `mcp-session`.

**Migration**: `scripts/check_images.sh check <dir>` compares renders bitwise at 16 spp;
`crust diff` compares two images.

### Requirement: Parity tests on the samples and import fixtures

**Reason**: The scene digest, the `parity` feature and the tests that compared every
import of the suite with a baseline are deleted, with
`crates/crust-render/tests/import_parity.rs`.

**Migration**: The import tests keep their own assertions, and the samples are guarded
by `scripts/check_images.sh`.

### Requirement: Large-scene parity, with memory and time reported

**Reason**: `scripts/import_parity.sh` compared ALab and Moana with the direct
importer's reports and images, which this change's Hydra layout departs from by design
(Moana's coral).

**Migration**: First-load time and peak memory on ALab and Moana are still measured
(`crust render --stats`) and recorded in the `usd-scene-import` design record, reported
and not gated.

### Requirement: Parity gates every step

**Reason**: The migration it gated is finished and the direct importer is gone.

**Migration**: A change to the import is gated, as any change is, on the image goldens
and the test suite.
