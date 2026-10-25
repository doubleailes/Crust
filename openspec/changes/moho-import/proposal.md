## Why

Crust reads USD in one place, `crust-core::scene::usd_import`, and every family module
there reads `openusd` prims directly. Mantle's `scene-index` program (step S2) moves
that reader onto Moho, a Hydra-2.0-style scene index over openusd that Mantle's IPR and
Obsidian also consume. Two readers of the same stage drift apart. With one index, an IPR
frame and `crust render -i` read the scene the same way, and S3's live updates get dirty
locators to work from. Moho's S1 core is done, with schemas for every family Crust
imports, so the importer can now migrate. The move is gated on parity with the direct
import, so nothing a render produces changes while it happens.

## What Changes

- **One openusd across the stack.** The workspace's `[patch.crates-io]` pins openusd and
  openusd-schemas by `rev = "68a867a69f3fb429a30f9e1155a0a30a3d2cdf9f"` instead of
  `branch = "main"`. Moho uses the same rev, and Cargo treats a `branch` source and a
  `rev` source as different crates, so without this the build would carry two openusd
  copies. Moho is added as a git dependency at
  `rev = "92c8b847822823f796b81b85452c3c400d77e5ec"`, and `deny.toml` allows its source.
- **A second import route, the index import.** The scene is read through a Moho scene
  index. For a small stage that is one standard chain. For a streamed stage it is a
  `MergingSceneIndex` over a payload-free scene input and one masked chunk input at a
  time. The importer pulls Hydra prims, schemas and generic attribute data (`crust:*`)
  instead of `usd::Prim`. It produces the same `Scene` as the direct import, through the
  same builders.
- **A route switch.** `CRUST_USD_IMPORT=direct|index` and a per-call override on
  `UsdImportOptions`. Until parity holds the default is `direct`, today's importer
  unchanged.
- **Module-by-module migration.** Each schema family (settings and products, camera,
  transforms and motion, meshes, implicit shapes, curves, materials, lights and light
  links, volumes, instancing) moves to the index on its own. Each move is gated by the
  parity check. Families not yet moved are read through a temporary bridge that hands
  the existing reader the `usd::Prim` at the same path. The bridge is deleted when the
  last family moves.
- **A parity check.** For every committed sample and the import tests' fixtures, the
  index import must equal the direct import. `scripts/import_parity.sh` compares the two
  routes on large scenes (ALab, Moana), using the `crust check` report, a bitwise 16 spp
  image and the `--stats` import memory and time. Moho's S0 harness (`moho-spike`) is the
  reference for streaming memory.
- **The default flips to `index`** once parity holds on the samples, ALab and Moana. The
  direct importer stays, selectable with `CRUST_USD_IMPORT=direct`. Removing it is a
  later change.
- **Moho-side gaps go to Moho.** Crust does not work around them silently. The gaps are
  cross-chunk light linking, materials outside the loaded chunks, authored-ness of scene
  globals and source paths for prototype prims (design, "Moho-side follow-ups").

Not in this change: S3, where Moho's dirty locators replace the layer-diff classifier
for live edits. In-place rebuilds keep today's classifier. Only the functions they call
move with their family, so an in-place update still equals a re-import.

## Capabilities

### New Capabilities

- `scene-index-import`: Crust reads a stage through the Moho scene index. This covers
  the route switch and its default, which hosts use it, streaming by chunk under a
  merging index within the direct import's memory bound, the index owning the time, and
  the rule that every family reads what the direct import reads.
- `import-parity`: the check that the index import equals the direct import. It defines
  what "equal" means (the scene, the `crust check` report, a bitwise image), where the
  check runs (tests on the samples and fixtures, and a script for ALab and Moana with
  memory and time bounds), and how a family's move is gated on it.

### Modified Capabilities

None. With the default `direct` the importer's behaviour is unchanged, and the index
route must reproduce it (`import-parity`). Behaviour changes that Moho's Hydra semantics
would bring, such as inheriting ancestors' constant primvars, are left for later
changes (design, Open Questions).

## Impact

- **Dependencies:** root `Cargo.toml` (`[patch.crates-io]` from branch to rev, a `moho`
  entry in `[workspace.dependencies]`), `Cargo.lock`, `deny.toml` (`allow-git` gains
  `https://github.com/doubleailes/Moho`). `crust-core` depends on `moho`. Bumping openusd
  goes in order: Moho, then crust-render, then Mantle. `cargo update -p openusd` no
  longer moves the pin.
- **Code:** `crates/crust-core/src/scene/usd_import/`. A new index route with its own
  traversal sits beside `traverse_into`, and each family module gains an index reader
  that feeds the same builders. `config.rs` gets the `CRUST_USD_IMPORT` field, and
  `UsdImportOptions` gets the route override. `usd_import/live.rs` rebuilds move with
  their families.
- **Hosts:** `crust render`, `check`, `diagnostic`, `crust mcp` and `crust-hydra` all
  import through `load_scene`, so they all follow the switch. hdCrust's `compare.sh` and
  `interaction.sh` exercise the index route once it is the default.
- **Tests and tooling:** a parity test in `crust-core/tests`, `scripts/import_parity.sh`,
  and recorded ALab and Moana baselines.
- **Docs:** `openspec/specs/usd-scene-import/design.md` (the index route, the bridge,
  gaps), `docs/architecture.md` (Moho in the crate map, the new switch), `CLAUDE.md`
  (openusd pinned by rev, the Moho rule, local co-development), and the site's
  environment-variables page.
- **Performance:** render throughput is untouched, because only the import changes.
  Import time and peak memory are gated on ALab and Moana (`import-parity`).
