## 1. The time

- [x] 1.1 Thread an optional frame through `scene::check_args` (`--frame`),
      `scene::import`, `Imported::update` (refused when it differs from the
      scene's time), `import_kept` and `update_or_import`; `crust mcp` passes
      none. Done when `check_args(.., Some(t))` parses to the CLI's
      `scene.frame == Some(t)` (`a_time_is_the_clis_frame`).
- [x] 1.2 `LiveSession::set_time`, applied by the next `load` / `apply` (a
      time change is `full`), and `Snapshot::time`.
- [x] 1.3 Tests (`session/live_tests.rs`): a time change renders the sampled
      translate at the new time, bitwise as a fresh session at that time, and
      the snapshot reports it; an edit at a time applies in place; a time that
      is not finite is refused; without a time the snapshot reports none.

## 2. Documentation

- [x] 2.1 The time in `live.rs`'s module docs and in the `mcp-session` design
      record's host-session section.
