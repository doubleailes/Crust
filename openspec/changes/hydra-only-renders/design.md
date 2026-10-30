## Context

`HostFacts` (`scene/hydra/index/host.rs`) is what the delegate asks its host
about the stage. The batch host and Mantle answer it with `StageHostFacts`
(`usd_import/stage_facts.rs`); `NoHostFacts` answers nothing. Of its eleven
questions, three change the committed scene:

- `authoring_layer_dir`, called through `asset_value_path`
  (`scene/hydra/assets.rs`) at every file-reading site: `UsdUVTexture` and
  Ptex files, MaterialX `filename`s, rect/dome light textures, IES profiles,
  the precomp kernel. `asset_value_path` takes the index value's
  `resolved_path` first. Moho resolves every relative `asset` value on its
  authoring layer (`stage/attribute.rs`, `anchor`), so the layer directory is
  only reached for a value authored as a `string`, or an asset Moho could not
  anchor (an anonymous layer). `0bebb347` added the string case to match the
  direct importer.
- `meters_per_unit`, read by `camera.rs` for the lens diameter.
- `mtlx_reference`, the `.mtlx` a material references, which
  `mtlx-format-plugin` replaces with composition.

`time_range` reads its values from the index already; only "is a range
authored" is the host's, and it only gates a warning.

## Goals / Non-Goals

**Goals:**
- With `NoHostFacts`, the delegate commits the same scene as with
  `StageHostFacts`, except a `.mtlx`-referenced material.
- That is checked by a test, not claimed.

**Non-Goals:**
- Removing the diagnostic facts. Hydra does not carry them, and a host that
  cannot answer them loses warnings, not pixels.
- MaterialX references: `mtlx-format-plugin` owns their removal.
- Changing the `HostFacts` trait's shape beyond removing two methods.

## Decisions

**D1. A `string` path resolves against the stage path's directory, with a
warning.** USD anchors `asset` values only; a `string` is text. The
delegate has one directory it may use without a stage: the one beside the
stage path its host gave (the root layer's, for the batch host).
`asset_value_path` loses its `layer_dir` argument and keeps its other
order: the resolved path, an absolute authored path, then the stage
directory. A relative `string` raises `asset.string_path` (`Approximated`,
logged each time the import reads it, as `texture.unreadable` counts each
reference to a file) naming the attribute and the resolved path, so a scene
that relied on layer anchoring says why its texture moved.
*Alternatives:* keep `authoring_layer_dir` as a declared exception (keeps a
non-USD behaviour that needs a stage); ask Moho to anchor `string` inputs
(non-USD in Moho, and every consumer would inherit it); refuse the path
(louder, but a string path beside the root layer works today and would
break needlessly).

**D2. The unit from `__usdMetersPerUnit/metersPerUnit`.** `camera.rs`'s
`authored_meters_per_unit` reads the index root (absent → `None`, the
existing "unauthored" warning and centimetre fallback). Bump Moho to the
merge of `stage-meters-per-unit` first. The value is read at each commit, as
the host fact was; Moho notifies nothing for a root metadata edit, which is
no worse than today (an in-place update did not re-ask the host either).

**D3. The guard renders through the delegate with both facts.** A test in
`crust-render` (it owns the delegate and the batch host's chain) opens each
`samples/*.usda` that references no `.mtlx`, builds the chain, commits once
with `StageHostFacts` and once with `NoHostFacts` on two delegates, renders
each at 16 spp at a reduced resolution (a `-r`-style override, so all
samples fit in the test budget), and compares RGBA bitwise. The stage path
given to both is the sample's, so D1's directory is the same. The `.mtlx`
samples (nine today) are listed by name, so a new one is excluded only on
purpose; `mtlx-format-plugin` empties the list.
*Alternative:* compare committed scenes structurally. Pixels are what the
requirement promises, and a structural diff would need an equality Crust
does not have.

**D4. `tests/asset_anchoring.rs`'s string case is rewritten, not deleted.**
It asserts D1: the file beside the root layer and the warning. The
`asset` cases keep their layer-anchored expectations, now met by the index
alone.

## Risks / Trade-offs

- [Production content authors texture paths as `string`] → Measured before
  merging: `crust check` on ALab and Moana counts `asset.string_path`. If
  either relies on it, this is the user's call before implementation goes
  further.
- [Mantle's mirror has no file, so its stage path is a fixed path under the
  current directory] → Mantle's LOPs author absolute paths; a `string` path
  there resolves under the current directory and warns, as any host
  without a layer.
- [The guard is slow] → Reduced resolution and the existing 16 spp pin; it
  renders each sample twice, once per facts.

## Migration Plan

1. Moho `stage-meters-per-unit` merged; bump Moho here.
2. D2, then D1 with its warning and test rewrite, then the guard (D3).
3. Docs: warnings page, design records, architecture.
4. Mantle bumps crust-render afterwards; it needs no code change.

## Open Questions

None left. Whether ALab or Moana author relative `string` paths (the first
risk) was measured on 2026-10-30 with a temporary count in
`asset_value_path` under `crust check`: ALab 2.3.0 (`entry.usda`, frame
1004) and the Moana island (`island.usda`) reach the layer directory zero
times, for `string` paths and for `asset` values Moho left unresolved
alike. The count fired on a sublayered `string inputs:file`, so a zero is a
real zero.
