## Why

Crust reads USD in one place, `crust-core::scene::usd_import`, and every family module
there reads `openusd` prims directly. Mantle's `scene-index` program (step S2) moves
that reader onto Moho, a Hydra-2.0-style scene index over openusd that Mantle's IPR and
Obsidian also consume. Two readers of the same stage drift apart. With one index, an IPR
frame and `crust render -i` read the scene the same way, and S3's live updates get dirty
locators to work from. Moho's S1 core is done, with schemas for every family Crust
imports, so the importer can now migrate. The move is gated on parity with baselines
recorded from today's importer, so nothing a render produces changes while it happens,
and the direct importer goes at the end of it.

## What Changes

- **One openusd across the stack.** The workspace's `[patch.crates-io]` pins openusd and
  openusd-schemas by `rev = "68a867a69f3fb429a30f9e1155a0a30a3d2cdf9f"` instead of
  `branch = "main"`. Moho uses the same rev, and Cargo treats a `branch` source and a
  `rev` source as different crates, so without this the build would carry two openusd
  copies. Moho is added as a git dependency at
  `rev = "92c8b847822823f796b81b85452c3c400d77e5ec"`, and `deny.toml` allows its source.
- **Baselines from today's importer, first.** Before anything migrates, the direct
  importer records baselines and they are kept for the whole change: a scene digest, the
  `crust check --json` report (timings excluded) and a 16 spp image hash for every
  committed sample and import test fixture, and for ALab and Moana the report, the EXR
  and the memory and time. A regenerate flag rewrites them. This is what UsdImaging did
  when it moved onto scene indices: the same baselines, a new route.
- **The importer reads through Moho.** The scene is read through a Moho scene index. For
  a small stage that is one standard chain. For a streamed stage it is `chain::merged`
  over a `MergingSceneIndex` of a payload-free scene input and one masked chunk input at
  a time. The importer pulls Hydra prims, schemas and generic attribute data (`crust:*`)
  instead of `usd::Prim`. It produces the same `Scene` as the direct importer did,
  through the same builders. There is one route and no switch.
- **Module-by-module migration.** Each schema family (settings and products, camera,
  transforms and motion, meshes, implicit shapes, curves, materials, lights and light
  links, volumes, instancing) moves to the index on its own, and each move is gated by
  parity with the baselines. While a family has not moved, a transitional bridge hands
  its existing reader the `usd::Prim` at the same path, so the tests stay green
  throughout. The bridge exists only inside this change.
- **The direct importer goes.** Once every family reads the index and parity holds on
  the samples, the fixtures, ALab and Moana, the direct readers, the bridge,
  `EvalTimeScope` and the prototype epoch are deleted. Every host (`crust render`,
  `check`, `diagnostic`, `mcp`, hdCrust) then imports through Moho.
- **A parity check.** `crust-core/tests/import_parity.rs` compares every sample with its
  baselines, and the import tests compare every fixture with theirs.
  `scripts/import_parity.sh record|check` does the same for large scenes (ALab, Moana),
  reporting memory and time without gating on them. Moho's S0 harness (`moho-spike`,
  strategies (B) and (D)) is the reference for streaming memory.
- **Moho-side gaps go to Moho.** Crust does not work around them silently. The gaps the
  plan found (cross-chunk light linking, materials outside the loaded chunks,
  authored-ness of scene globals, source paths for prototype prims, an index strategy in
  the spike, pass collections, merged children order) are closed in Moho's change
  `scene-index-gaps`. Crust builds against it and bumps the `moho` rev once it merges
  (design, "Moho-side follow-ups").

Not in this change: S3, where Moho's dirty locators replace the layer-diff classifier
for live edits. In-place rebuilds keep today's classifier. Only the functions they call
move with their family, so an in-place update still equals a re-import.

## Capabilities

### New Capabilities

- `scene-index-import`: Crust reads every stage through the Moho scene index, the one
  import route. This covers the hosts that import through it, streaming by chunk under
  a merging index within the direct importer's memory bound, the index owning the time,
  and the rule that the import reads what the direct importer read.
- `import-parity`: the check that the import through Moho equals baselines recorded
  from the direct importer. It defines the baselines and their regeneration, what
  "equal" means (the scene, the `crust check` report, a bitwise image), where the check
  runs (tests on the samples and fixtures, and a script for ALab and Moana with memory
  and time reported), and how each family's move and the direct importer's removal are
  gated on it.

### Modified Capabilities

None. The import through Moho must reproduce the direct importer's behaviour exactly
(`import-parity`), so `usd-scene-import`'s requirements are unchanged; its design record
is updated. Behaviour changes that Moho's Hydra semantics would bring, such as
inheriting ancestors' constant primvars, are left for later changes (design, Open
Questions).

## Impact

- **Dependencies:** root `Cargo.toml` (`[patch.crates-io]` from branch to rev, a `moho`
  entry in `[workspace.dependencies]`), `Cargo.lock`, `deny.toml` (`allow-git` gains
  `https://github.com/doubleailes/Moho`). `crust-core` depends on `moho`. Bumping openusd
  goes in order: Moho, then crust-render, then Mantle. `cargo update -p openusd` no
  longer moves the pin.
- **Code:** `crates/crust-core/src/scene/usd_import/`. A new `index/` module with its own
  walk replaces `traverse_into`, each family module gains an index reader that feeds the
  same builders, and the direct readers, `time.rs` (`EvalTimeScope`) and the prototype
  epoch are deleted at the end. `usd_import/live.rs` rebuilds move with their families.
  `listing.rs` (`crust ls`) is not an import and stays.
- **Hosts:** `crust render`, `check`, `diagnostic`, `crust mcp` and `crust-hydra` all
  import through `load_scene`, so they all read through Moho. hdCrust's `compare.sh`,
  `interaction.sh` and `usdc_roundtrip.sh` exercise it.
- **Tests and tooling:** a scene digest behind a test-only feature, `import_parity.rs`
  and recorded baselines under `crust-core/tests/data/import_baselines/`, a baseline
  helper for the import tests, `scripts/import_parity.sh`, and recorded ALab and Moana
  baselines.
- **Docs:** `openspec/specs/usd-scene-import/design.md` (the import through Moho, the
  baselines, the closed gaps), `docs/architecture.md` (Moho in the crate map),
  `CLAUDE.md` (openusd pinned by rev, the Moho rule, local co-development, the retired
  `EvalTimeScope` and epoch rules), and a site architecture page on the import.
- **Performance:** render throughput is untouched, because only the import changes.
  Import time and peak memory are measured on ALab and Moana and reported against the
  direct importer's baselines, not gated (`import-parity`), as UsdImaging's move onto
  scene indices was gated on parity alone.
