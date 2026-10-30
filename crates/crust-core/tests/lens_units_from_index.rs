//! `hydra-only-renders` "The unit from the index": the lens converts its
//! millimetres through the stage unit the index carries
//! (`__usdMetersPerUnit/metersPerUnit`, Moho), so a host that answers no
//! facts about its stage commits the same lens as one that does.

use std::path::Path;
use std::sync::Arc;

use crust_core::{HostFacts, NoHostFacts, Scene, SceneDelegate, StageHostFacts, UsdImportOptions};
use moho::openusd::usd::{InitialLoadSet, Stage};
use moho::stage::StageSceneIndex;

const METRE_LENS: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "World"
{
    def Camera "cam"
    {
        float focalLength = 50
        float fStop = 2
        float focusDistance = 5
    }

    def Sphere "ball"
    {
        double radius = 1
        double3 xformOp:translate = (0, 0, -5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;

/// The scene a fresh delegate commits over the stage at `path`, with
/// `facts` built from the stage when `stage_facts` is set, else none.
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
        Arc::new(crust_core::NoAssets),
    );
    delegate.update(facts.as_ref(), None).unwrap().scene
}

/// The distance between two opposite lens samples through the frame centre:
/// the lens diameter, in scene units.
fn lens_diameter(scene: &Scene) -> f32 {
    let a = scene.camera.get_ray(0.5, 0.5, [0.0, 0.5], 0.0);
    let b = scene.camera.get_ray(0.5, 0.5, [1.0, 0.5], 0.0);
    a.origin().distance(b.origin())
}

#[test]
fn a_metre_stage_has_the_same_lens_without_host_facts() {
    let dir = std::env::temp_dir().join("crust_lens_units_from_index");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metre_lens.usda");
    std::fs::write(&path, METRE_LENS).unwrap();

    let with = commit(&path, true);
    let without = commit(&path, false);
    // 50 mm at f/2 is 25 mm: 0.025 units on a metre stage (2.5 on the
    // centimetre fallback, had the unit been lost).
    assert!(
        (lens_diameter(&with) - 0.025).abs() < 1e-7,
        "{}",
        lens_diameter(&with)
    );
    assert_eq!(
        lens_diameter(&with).to_bits(),
        lens_diameter(&without).to_bits(),
        "with host facts {}, without {}",
        lens_diameter(&with),
        lens_diameter(&without)
    );
}
