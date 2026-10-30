## 0. Before starting

- [x] 0.1 Run `crust check --json -` on ALab and Moana and count relative paths authored as `string` at the file-reading sites (a temporary debug count, not committed); record the counts in design.md's open question, and stop for the user's decision if either is non-zero

## 1. The unit from the index (design D2)

- [x] 1.1 Bump Moho to the merge of `stage-meters-per-unit` (`[workspace.dependencies]`), `cargo update -p moho`; verify `cargo tree -i openusd` shows one copy
- [x] 1.2 `camera.rs`: read `__usdMetersPerUnit/metersPerUnit` from the index root instead of `HostFacts::meters_per_unit`; remove the method from `HostFacts`, `NoHostFacts` and `StageFacts`; the existing depth-of-field and refused-unit tests pass unchanged
- [x] 1.3 Add a test: a `metersPerUnit = 1` stage with depth of field commits the same lens with `NoHostFacts` as with `StageHostFacts` (spec "The unit from the index")

## 2. Asset paths from the index (design D1, D4)

- [x] 2.1 Add `WarningCode::AssetStringPath` (`asset.string_path`, `Approximated`, each read) and its row in `site/content/docs/reference/warnings.md`; the warnings-in-step test passes
- [x] 2.2 `asset_value_path` drops its `layer_dir` argument: resolved path, absolute authored path, then the stage path's directory, raising `asset.string_path` for a relative `string`; update its call sites in `lights.rs`, `materials.rs`, `products.rs`/precomp and `mod.rs`, and remove `layer_dir` helpers
- [x] 2.3 Remove `HostFacts::authoring_layer_dir` and `stage_facts::authoring_layer_dir`
- [x] 2.4 Rewrite `tests/asset_anchoring.rs`'s string case to D1 (the file beside the root layer, the warning raised), add an absolute-string case without warning; verify every `asset` case passes unchanged

## 3. The guard (design D3)

- [x] 3.1 Add `crates/crust-render/tests/host_facts.rs` (feature `delegate`): every `samples/*.usda` not on a named `.mtlx` list, committed through two delegates (`StageHostFacts`, `NoHostFacts`), rendered at 16 spp at a reduced resolution, compared bitwise (spec "No host facts, the same image")
- [x] 3.2 In the same file, the listed exception: a `.mtlx`-referencing sample differs with `NoHostFacts` (spec "The listed exception"), so the list cannot hide a regression silently
- [x] 3.3 Run `scripts/check_images.sh check` against goldens recorded before 1.2 (16 spp, `--indirect-clamp 0` on both sides); verify no sample's image changed

## 4. Documentation and gate

- [x] 4.1 `openspec/specs/render-delegate/design.md` and `usd-scene-import/design.md`: host facts are diagnostics only, with the `.mtlx` exception; the string-path anchoring history and its reversal; `docs/architecture.md`'s delegate section likewise
- [x] 4.2 `site/`: the asset-path page states that `string` paths resolve against the stage's directory; `zola build` passes
- [x] 4.3 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p crust-render --features delegate --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast`; verify all pass

## Workflow follow-up

- Mantle bumps crust-render to this change's merge; no Mantle code changes.
- `mtlx-format-plugin` empties the guard's `.mtlx` list and removes `mtlx_reference`.
