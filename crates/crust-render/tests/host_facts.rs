//! `hydra-only-renders`: the image depends on the scene index alone. Every
//! sample that references no `.mtlx` document renders bit-identically when
//! its delegate is told nothing about the stage (`NoHostFacts`) and when it
//! is told everything (`StageHostFacts`): what the host knows of its stage
//! only informs warnings, reports and provenance. The one listed exception,
//! a material referencing a `.mtlx` document (which openusd does not
//! compose), is checked to differ, so the list cannot hide a regression.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crust_assets::FileAssets;
use crust_core::{
    HostFacts, NoHostFacts, Renderer, Scene, SceneDelegate, StageHostFacts, UsdImportOptions,
};
use moho::openusd::usd::{InitialLoadSet, Stage};
use moho::stage::StageSceneIndex;

/// The samples whose materials reference a `.mtlx` document: the one fact
/// still asked of the host (`HostFacts::mtlx_reference`) until
/// `mtlx-format-plugin` composes them into the stage.
const MTLX_REFERENCING: &[&str] = &[
    "displacement",
    "hair",
    "hair_variation",
    "materialx_basic",
    "materialx_cutout",
    "materialx_emissive",
    "materialx_subsurface",
    "materialx_surfaces",
    "thin_window",
];

/// The resolution every sample is rendered at: small enough for an
/// unoptimized test build, large enough to see every texture. Both sides of
/// a comparison render at it, so the frame's own aspect does not matter.
const RESOLUTION: (usize, usize) = (24, 16);

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

/// The scene a fresh delegate commits over `path`, told about the stage when
/// `stage_facts` is set, loading assets from disk.
fn commit(path: &Path, stage_facts: bool) -> Scene {
    let stage = Stage::builder()
        .schema_registry(crust_core::usd_schema_registry())
        .load(InitialLoadSet::LoadAll)
        .open(path.to_str().unwrap())
        .unwrap();
    let index = StageSceneIndex::new();
    index.set_stage(stage.clone());
    let chain = moho::chain::standard(index);
    let facts: Box<dyn HostFacts> = if stage_facts {
        Box::new(StageHostFacts::new(&stage, path, Some(chain.clone())))
    } else {
        Box::new(NoHostFacts)
    };
    let mut delegate = SceneDelegate::new(
        chain,
        path,
        UsdImportOptions::default(),
        Arc::new(FileAssets::new()),
    );
    delegate.update(facts.as_ref(), None).unwrap().scene
}

/// The scene rendered at 16 spp and [`RESOLUTION`], as bits.
fn bits(scene: Scene) -> Vec<u32> {
    let settings = scene
        .settings
        .with_samples_per_pixel(16)
        .with_resolution(RESOLUTION.0, RESOLUTION.1);
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes);
    let (image, _) = renderer.render_with_stats(true, &|_, _| {});
    let (w, h) = image.size();
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let (r, g, b) = image.get_rgb(x, y);
            [r.to_bits(), g.to_bits(), b.to_bits()]
        })
        .collect()
}

fn same_without_host_facts(path: &Path) -> bool {
    bits(commit(path, true)) == bits(commit(path, false))
}

#[test]
fn no_host_facts_render_the_same_image() {
    let mut checked = Vec::new();
    let mut differ = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(samples())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "usda"))
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if MTLX_REFERENCING.contains(&name.as_str()) {
            continue;
        }
        if !same_without_host_facts(&path) {
            differ.push(name.clone());
        }
        checked.push(name);
    }
    assert!(checked.len() > 30, "{checked:?}");
    assert!(differ.is_empty(), "differ without host facts: {differ:?}");
}

/// The listed exception: without the host, a `.mtlx`-referenced material
/// composes empty and renders otherwise.
#[test]
fn a_mtlx_reference_still_needs_the_host() {
    assert!(!same_without_host_facts(
        &samples().join("materialx_basic.usda")
    ));
}
