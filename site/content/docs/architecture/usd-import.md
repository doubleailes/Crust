+++
title = "The USD import"
description = "How Crust Render reads a USD stage: through Moho, a scene index in the model of Hydra 2.0, one subtree at a time."
date = 2026-10-27T08:00:00+00:00
updated = 2026-10-27T08:00:00+00:00
draft = false
weight = 15
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render reads every stage through Moho, a scene index over openusd built on the model of Hydra 2.0, and streams a large stage one subtree at a time.'
toc = true
top = false
+++

## Through a scene index

The renderer never walks USD prims itself. Every stage is read through
[Moho](https://github.com/doubleailes/Moho), a scene index over the pure-Rust
[openusd](https://github.com/mxpv/openusd) crate, on the model of Hydra 2.0, the scene
description layer of Pixar's OpenUSD:

- a **scene index** gives each prim a Hydra type (`mesh`, `camera`, `sphereLight`, …) and
  its data as **data sources**, read lazily;
- **filters** stack on top of it, as UsdImaging stacks them. Native instancing and point
  instancing produce prototypes, flattening composes transforms and visibility,
  material binding resolution answers which material each prim renders with, and
  light linking turns collections into categories;
- the index owns the **time**, so a render at `-f 12` reads every value at frame 12, and
  motion blur reads transforms at the shutter's offsets around it.

Crust walks the top of that stack in namespace order and hands each prim, by its Hydra
type, to the reader for its family: meshes, curves, implicit shapes, lights, cameras,
materials, instancers, volumes, render settings and products. A reader takes the
schema's data where Hydra carries what Crust needs. Elsewhere it reads the attribute
values Moho carries alongside, with whether each is authored. That is where Crust
reads its own `crust:*` attributes.

A few facts are not scene description: the layers a stage composed, an override
authored to hide a prim, where a material's specs come from. Crust reads those from the
stage itself, in one place, and nowhere else.

## One stage

Crust reads one stage, payloads loaded, as a Hydra render delegate is handed one scene
index. Moho composes the Moana Island whole at about 7 GiB, so there is no chunking.
Light links are judged after the walk, once every light has been seen.

## Prototypes

Moho gives each instanced prototype one path. A prototype is therefore built once for
the whole render and placed wherever it is used, whichever instance or point instancer
places it. Warnings and the material provenance an
MCP session reports still name the prims by their USD paths, not by the index's.

## One openusd

Crust reaches openusd only through Moho, which pins it to one revision, the one Mantle
uses too. A bump moves Moho first, then Crust Render, then Mantle.
