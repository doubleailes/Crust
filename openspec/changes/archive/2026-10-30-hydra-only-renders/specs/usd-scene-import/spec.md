## ADDED Requirements

### Requirement: Relative asset paths resolve as the index gives them

A file named by an `asset`-typed value (a texture, a Ptex file, a MaterialX
`filename`, a light texture, an IES profile, the precomp kernel) SHALL be the
path the index's value resolves to, which is anchored on the layer that
authors it. A relative path authored as a `string`, which USD does not
anchor, SHALL resolve against the directory of the stage path the host gave,
and SHALL raise the `asset.string_path` warning, naming the attribute and the
path it resolved to, each time the import reads it (as `texture.unreadable`
counts each reference to a file).

#### Scenario: An asset in a referenced layer

- **WHEN** a layer two directories below the root layer authors
  `asset inputs:file = @textures/rough.png@`
- **THEN** the texture read is `textures/rough.png` beside that layer

#### Scenario: A string in a referenced layer

- **WHEN** the same layer authors `string inputs:file = "textures/rough.png"`
- **THEN** the texture read is `textures/rough.png` beside the root layer, and
  `asset.string_path` names the attribute and that path

#### Scenario: An absolute string

- **WHEN** a layer authors a `string` file path that is absolute
- **THEN** that path is read and no warning is raised
