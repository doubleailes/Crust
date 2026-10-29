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
- `--variance-threshold`, `--min-samples`, `--neighbour-tolerance`.

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

### Requirement: Listing what a stage holds

`crust ls <kind> -i <scene>` SHALL print the stage's prims of `kind`, one
absolute path per line on stdout, in namespace order, found from the batch host's
scene index (the chain a render builds) by Hydra prim type, so that the list is
what a render would use. `kind` SHALL be one of
`camera`, `light`, `material` or `pass`, each also accepted in the plural:

- `camera`: exactly the cameras the render can go through — none under an
  inactive, abstract or proxy- / guide-purpose ancestor, inside an instance's
  prototype or beneath a `PointInstancer`; those under an invisible ancestor
  included.
- `light`: the UsdLux lights the import reads (sphere, rect, disk, cylinder,
  distant, dome), pruned as geometry is, invisibility included. A light the
  import refuses for its evaluated values (a zero radius or size, a transform
  that collapses it) SHALL still be listed: membership does not depend on the
  time code, even when `-f` is given.
- Each path SHALL be listed once.
- `material`: every `UsdShadeMaterial` a binding can reach — all but those
  under an inactive ancestor or inside an instance's prototype.
- `pass`: every `RenderPass` prim `--pass` accepts — all but those under an
  inactive ancestor.

Its log SHALL go to stderr, so stdout holds only the list.

#### Scenario: Listing the cameras of a stage

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda`
- **THEN** stdout is `/scene/camera1` and the exit status is 0

#### Scenario: Listing the passes of a stage

- **WHEN** a stage defines `/Render/Passes/bg` and `/Render/Passes/char`, and the
  user runs `crust ls pass -i shot.usda`
- **THEN** stdout is those two paths, one per line, in namespace order

#### Scenario: An unknown kind

- **WHEN** the user runs `crust ls mesh -i scene.usda`
- **THEN** the arguments are refused as a usage error naming the valid kinds

#### Scenario: A stage that cannot be opened

- **WHEN** the input does not exist
- **THEN** an error is logged and the exit status is non-zero

#### Scenario: The text listing is unchanged by metadata

- **WHEN** the user runs `crust ls light -i scene.usda` without `--json`
- **THEN** stdout holds only the lights' paths, one per line, with no metadata

### Requirement: Listing at a time code

`crust ls` SHALL accept `-f/--frame`, a time code at which every listed value
is evaluated, with the same parsing as `crust render -f`. It SHALL set the batch
host's index time to it. Without it, values SHALL be read at the index's time code 0,
as a render without `-f` reads them (`scene-index-import`, "The index owns the
time"), not at their default (non-time-sampled) value. A listed attribute that authors
both a default and time samples is reported by `crust check` with the
`time.default_and_samples` warning; `crust ls` does not report it.

#### Scenario: An animated light intensity

- **WHEN** a light's `inputs:intensity` has time samples 1 at frame 1 and 4
  at frame 10, and the user runs `crust ls light -i scene.usda -f 10 --json -`
- **THEN** that light's `intensity` is 4

#### Scenario: A default beside a sample at 0

- **WHEN** a light's `inputs:intensity` authors `default = 2` and
  `timeSamples = {0: 3}`, and the user runs `crust ls light -i scene.usda --json -`
  without `-f`
- **THEN** that light's `intensity` is 3, the value a render without `-f` uses, and
  `crust check` on the same stage raises `time.default_and_samples` for that attribute
