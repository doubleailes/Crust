+++
title = "The USD import"
description = "How Crust Render reads a USD stage: through Moho, a scene index in the model of Hydra 2.0."
date = 2026-10-27T08:00:00+00:00
updated = 2026-10-29T08:00:00+00:00
draft = false
weight = 15
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render reads every stage through Moho, a scene index over openusd built on the model of Hydra 2.0.'
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

Crust is a **render delegate** over that stack, as hdPrman or hdEmbree are over
UsdImaging's. It observes the index's prims as they are added, removed and dirtied,
keeps one managed prim per prim, and commits them into a scene in path order: meshes,
curves, implicit shapes, lights, cameras, materials, instancers, volumes, render settings
and products, each by its Hydra type. It reads the Hydra schemas only. That is why its
own `crust:*` attributes are constant primvars on geometry and volumes, which Hydra
carries, and why a missing value reads as the schema's default.

A few facts are not scene description: the layers a stage composed, an override
authored to hide a prim, where a material's specs come from. The program that opened
the stage, the *host*, answers those. For `crust render` that is the command line.
The delegate itself never names a stage.

## One stage

Crust reads one stage, payloads loaded, as a Hydra render delegate is handed one scene
index. Moho composes the Moana Island whole at about 7 GiB, so there is no chunking.
Light links are installed after the commit, once every light has been seen.

## Updates

A host that keeps the delegate, such as Mantle's viewport, edits the stage and asks
for an update. The delegate reads what the index reports as changed. Render settings
and the camera, a material's inputs, a light's parameters, a light added or removed,
a light's or a receiver's light links, an area light moved, and a mesh's transform or
points (a mesh another prim shares, and a mesh inside an instanced prototype,
included) are updated in place. Anything else is committed again from the prims it
keeps, with its textures still decoded, after the scene it replaces is let go. Either
way the scene is exactly the one a fresh render would read.

The lights are a layer of their own: their geometry is numbered after every other
geometry and kept in a part of the acceleration structure of its own, so a light
added, removed or moved rebuilds the lights alone. A scene with area lights pays a
little for it on every ray (usdlux renders about 7% slower).

## Prototypes

Moho gives each instanced prototype one path. A prototype is therefore built once for
the whole render and placed wherever it is used. Warnings still name the prims by their
USD paths, not by the index's.

## One openusd

Crust reaches openusd only through Moho, which pins it to one revision, the one Mantle
uses too. A bump moves Moho first, then Crust Render, then Mantle.
