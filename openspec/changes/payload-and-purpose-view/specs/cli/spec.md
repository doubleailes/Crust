# Spec Delta

## MODIFIED Requirements

### Requirement: Subcommands

The CLI SHALL require a subcommand:

- `render`, which renders;
- `ls`, which lists;
- `check`, which imports a stage as a render would and reports on it without
  rendering (see the `scene-check` capability);
- `diff`, which compares two EXRs (see the `image-comparison` capability);
- `diagnostic`, which measures and reports how to make the render faster
  or cleaner (see the `diagnostics` capability);
- `light-paths`, which re-renders a small region and reports the light paths
  its samples took (see the `light-path-inspection` capability).

The render flags, `--log-file` among them, SHALL belong to `render`. The
flags that shape the scene and its settings SHALL also be accepted by
`diagnostic`, `check` and `light-paths`, with the same names, values and
defaults:

- `-i`, `-f`, `--camera`, `--region`;
- `--strategy`, `--light-selection`, `--light-samples`,
  `--light-samples-indirect`;
- `--indirect-clamp`, `--filter`, `--filter-radius`, `--sampler`;
- `--subdiv-level`, `--subdiv-edge-length`, `--auto-tx`, `--tx-color-space`;
- `--variance-threshold`, `--min-samples`, `--neighbour-tolerance`;
- `--purposes`, `--binding-purposes`, `--unloaded`, `--load`, `--unload`.

`light-paths` SHALL also accept `render`'s `-s`, so that it reproduces a render
made at that sample count.

`-l/--level` SHALL be accepted by every subcommand, before or after its
name. `--log-file`'s directory is optional, so it SHALL NOT be accepted
where it could take a subcommand's name or a positional as that directory.

#### Scenario: No subcommand

- **WHEN** the user runs `crust -i scene.usda`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: Render-only flags are refused by diagnostic

- **WHEN** the user runs `crust diagnostic -i scene.usda -s 64`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: A shared flag means the same in both

- **WHEN** the user runs
  `crust diagnostic -i scene.usda --light-selection learned`
- **THEN** the report's effective settings show `light_selection = learned`,
  as a render with the same flag would use

#### Scenario: The sampler is a shared flag

- **WHEN** the user runs `crust check -i scene.usda --sampler sobolBn --json -`
- **THEN** the report's effective settings show `sampler` as `sobolBn`, with flag
  `--sampler` and attribute `crust:sampler`

#### Scenario: An unknown sampler on the command line

- **WHEN** the user runs `crust render -i scene.usda --sampler halton`
- **THEN** the arguments are refused as a usage error listing the six accepted values, and
  nothing is loaded

#### Scenario: Comparing two images

- **WHEN** the user runs `crust diff a.exr b.exr`
- **THEN** the two files are compared and nothing is rendered

#### Scenario: Render-only flags are refused by check

- **WHEN** the user runs `crust check -i scene.usda -o out.exr`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: No MCP server

- **WHEN** the user runs `crust mcp`
- **THEN** the arguments are refused as a usage error naming the valid subcommands

#### Scenario: Inspecting light paths at the render's sample count

- **WHEN** the user runs
  `crust light-paths -i scene.usda -s 64 --region 40,40,50,50`
- **THEN** the region is rendered at 64 samples per pixel, as `crust render -s 64`
  would render it

#### Scenario: Output flags are refused by light-paths

- **WHEN** the user runs `crust light-paths -i scene.usda --region 0,0,5,5 -o out.exr`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: The adaptive flags are shared

- **WHEN** the user runs
  `crust diagnostic -i scene.usda --variance-threshold 0.02 --min-samples 16`
- **THEN** the report's effective settings show `variance_threshold = 0.02` and
  `min_samples_per_pixel = 16`, as a render with the same flags would use

#### Scenario: The .tx target space is a shared flag

- **WHEN** the user runs `crust check -i scene.usda --tx-color-space "Linear Rec.709 (sRGB)"`
- **THEN** the `.tx` files the check looks for, and those `--auto-tx` would write, are
  named for `Linear Rec.709 (sRGB)` as the target space, as a render with the same
  flag would name them

#### Scenario: An unknown .tx target space

- **WHEN** the user runs `crust render -i scene.usda --tx-color-space nonsense`
- **THEN** the arguments are refused with an error saying the OCIO config defines no
  such colour space, and nothing is loaded

#### Scenario: Checking a decision view

- **WHEN** the user runs `crust check -i scene.usda --unloaded --purposes proxy`
- **THEN** the report describes the scene that
  `crust render -i scene.usda --unloaded --purposes proxy` would render


## ADDED Requirements

### Requirement: Choosing the drawn purposes

`--purposes <list>` SHALL take a comma-separated list of `render`, `proxy` and `guide`
(`default` is accepted and always implied). It SHALL replace the stage's
`includedPurposes` for this run. An unknown token SHALL be a usage error naming the
accepted values.

#### Scenario: Proxies instead of render geometry

- **WHEN** the user runs `crust render -i scene.usda --purposes proxy`
- **THEN** proxy-purpose geometry renders and render-purpose geometry does not

#### Scenario: A misspelt purpose

- **WHEN** the user passes `--purposes proxi`
- **THEN** argument parsing fails, naming `render`, `proxy` and `guide`, and nothing is
  loaded

### Requirement: Choosing the material binding purposes

`--binding-purposes <list>` SHALL take a comma-separated, ordered list of `full`,
`preview` and `all` (the all-purpose binding). It SHALL replace the stage's
`materialBindingPurposes` for this run. An unknown token SHALL be a usage error naming
the accepted values.

#### Scenario: Preview materials

- **WHEN** the user runs `crust render -i scene.usda --binding-purposes preview,all`
- **THEN** each prim renders with its `preview` binding where it has one, and with its
  all-purpose binding otherwise

### Requirement: Choosing the payloads loaded

`--unloaded` SHALL start from no payload loaded instead of every one. `--load <prim>` and
`--unload <prim>`, each repeatable and in any order, SHALL load or unload the payloads at
and below each named prim. Naming one prim in both SHALL be a usage error. A `--load`
that changes nothing, because nothing above the prim is unloaded, SHALL be reported as a
warning.

#### Scenario: A decision image of a heavy stage

- **WHEN** the user runs
  `crust render -i shot.usda --unloaded --purposes proxy --binding-purposes preview,all`
  on a stage whose assets publish proxies outside their payloads
- **THEN** the image shows every asset's proxy with its preview material, and no
  payload layer is opened

#### Scenario: Judging one asset's geometry

- **WHEN** the user runs
  `crust render -i shot.usda --unloaded --load /World/hero --purposes render,proxy`
- **THEN** only `/World/hero`'s payload is loaded and it is drawn by its render
  geometry, while the other assets stay unloaded and are drawn by their proxies

#### Scenario: Leaving out one heavy asset

- **WHEN** the user runs `crust render -i shot.usda --unload /World/forest`
- **THEN** every payload but `/World/forest`'s and those below it is loaded

#### Scenario: A prim both loaded and unloaded

- **WHEN** the user passes `--load /World/a --unload /World/a`
- **THEN** argument parsing fails, naming `/World/a`, and nothing is loaded

#### Scenario: A load with nothing to undo

- **WHEN** the user passes `--load /World/a` without `--unloaded` or an `--unload` above
  it
- **THEN** the render loads every payload, and the log warns that `--load /World/a`
  changed nothing
