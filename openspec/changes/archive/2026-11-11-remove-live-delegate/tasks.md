## 1. Code

- [x] 1.1 Remove `crust-render`'s `delegate` feature, its test, the `update_latency` example and the CI steps
- [x] 1.2 Make `SceneDelegate` commit afresh on each call; remove the update classes and `scene/hydra/update.rs`
- [x] 1.3 Remove `crust_core::live`, `crust_core::place`, `Scene::provenance` and what the import recorded for them
- [x] 1.4 Remove the world's kept geometry and in-place replacement, the light list's replacement and the shared environment maps
- [x] 1.5 Remove `crust-assets`' kept decodes and `crust-render`'s `Assets::revalidate` / `sweep`
- [x] 1.6 Remove `CRUST_SCENE_DELTAS`

## 2. Documentation

- [x] 2.1 Update `CLAUDE.md`, `docs/architecture.md` and the `render-delegate` design record
- [x] 2.2 Remove `CRUST_SCENE_DELTAS` from the user site, and the live-host wording from its pages

## 3. Verification

- [x] 3.1 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- [x] 3.2 `scripts/check_images.sh check` against goldens recorded before the change: every scene bit-identical
- [x] 3.3 `openspec validate remove-live-delegate`: no errors (two length warnings, on requirement text kept verbatim)
