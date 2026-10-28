## 1. Pick

- [x] 1.1 `Imported::name_hit` and `Stale` (`session/scene.rs`); `probe`'s
      `hit_names` through it. Done when the `probe` tests pass unchanged.
- [x] 1.2 `LiveSession::pick`, `Pick`, and the render's camera, frame, region
      and geometry on `Running`. Tests (`session/live_tests.rs`): a hit naming
      the prim and its material, a PointInstancer and a native instance naming
      the instancer and instance prims, a miss, a pixel outside the image;
      `Busy` after a failed load dropped the scene; `name_hit` refusing another
      import's geometry.

## 2. The camera retarget in the rebuild (crust-core)

- [x] 2.1 `StageFacts::cameras`; `refresh_settings` resolves the target and
      refuses an unmet camera, a camera with an imported prim below it, another
      shutter; `SettingsUpdate` carries the camera path and shutter and
      `Update::apply` writes them. Tests (`tests/live.rs`): a retarget, and a
      retarget there and back, bitwise a fresh import at 16 spp; refusals for a
      non-camera, a carrying camera, another shutter, an unnamed fallback to a
      camera with another shutter, an adaptive scene with no dicing camera; in
      place with one.

## 3. The classifier (crust-render)

- [x] 3.1 `LayerChange::Targets`, and `classify` admitting `camera` on the
      settings prim or a `RenderProduct`. Unit tests in `session/delta.rs`.
- [x] 3.2 `mcp/tests.rs`: retargeting the settings' and the product's camera,
      moving the new camera and the undos are `settings` and bitwise a full
      import's; a non-camera, a carrying camera and an adaptive scene with no
      dicing camera are `full`; a diced adaptive scene is `settings`.
- [x] 3.3 `session/live_tests.rs`: Mantle's `ipr.usda` rewritten with another
      camera on the settings and its products is `settings`, bitwise a fresh
      session's at 16 spp, and so is switching back.

## 4. Docs

- [x] 4.1 Design records (`mcp-session`, `usd-scene-import`),
      `docs/architecture.md`, and the user pages (`help/claude-desktop.md`,
      `architecture/limitations.md`).

## 5. Gates

- [x] 5.1 `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`.
- [x] 5.2 `zola build` in `site/` passes its link check (Zola 0.18 here, not
      the pinned 0.21).
- [ ] 5.3 `cargo deny --locked check` and `scripts/check_images.sh` against
      goldens recorded before the change, where those tools are installed. The
      render path is untouched.

## Workflow follow-up

- Archive after `live-geometry-edits`, whose text of "Edits apply in place
  when they are confined" this change's delta builds on.
