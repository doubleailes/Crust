//! `host-facts-mtlx-only`: the delegate asks its host for a material's `.mtlx`
//! reference and nothing else; every warning and error it raises about the
//! scene is decided from the index. These tests drive a delegate with no
//! stage facts at all (`NoHostFacts`), as a Hydra host would.

#![allow(clippy::unwrap_used)]

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crust_core::{
    HostFacts, NoHostFacts, Scene, SceneDelegate, UsdImportOptions, Warning, WarningCode,
};
use moho::chain::RendererFilter;
use moho::filter::SceneGlobalsSceneIndex;
use moho::openusd::sdf;
use moho::openusd::usd::{InitialLoadSet, Stage, TimeCode};
use moho::stage::StageSceneIndex;

fn write(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_host_facts_mtlx_only");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// What a fresh delegate commits over the stage at `path` at `frame`, under
/// `options`, told nothing about the stage.
fn commit(path: &Path, frame: f64, options: UsdImportOptions) -> Result<Scene, crust_core::Error> {
    let stage = Stage::builder()
        .schema_registry(crust_core::usd_schema_registry())
        .load(InitialLoadSet::LoadAll)
        .open(path.to_str().unwrap())
        .unwrap();
    let index = StageSceneIndex::new();
    index.set_time(TimeCode::new(frame));
    index.set_stage(stage);
    let cell: Rc<OnceCell<Rc<SceneGlobalsSceneIndex>>> = Rc::default();
    let captured = cell.clone();
    let chain = moho::chain::renderer_filters(
        moho::chain::standard(index),
        &[RendererFilter::scene_globals(move |g| {
            let _ = captured.set(g);
        })],
    );
    cell.get().unwrap().set_current_frame(frame);
    let mut delegate = SceneDelegate::new(chain, path, options, Arc::new(crust_core::NoAssets));
    delegate.update(&NoHostFacts, None).map(|u| u.scene)
}

fn outside_range(scene: &Scene) -> Option<&Warning> {
    scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::TimeOutsideRange)
}

const CAMERA: &str = r#"def Camera "cam"
{
}
"#;

/// `HostFacts` asks one thing: a host answers a material's `.mtlx` reference
/// and has nothing else to implement (spec "One question").
#[test]
fn a_host_answers_one_question() {
    struct OnlyMtlx;
    impl HostFacts for OnlyMtlx {
        fn mtlx_reference(&self, _: &sdf::Path) -> Option<(PathBuf, String)> {
            None
        }
    }
    let facts: &dyn HostFacts = &OnlyMtlx;
    assert!(facts.mtlx_reference(&sdf::Path::abs_root()).is_none());
}

/// The time range is the index's (spec "A frame outside the range").
#[test]
fn a_frame_outside_an_authored_range_warns_without_host_facts() {
    let path = write(
        "range.usda",
        &format!("#usda 1.0\n(\n    startTimeCode = 1001\n    endTimeCode = 1050\n)\n{CAMERA}"),
    );
    let scene = commit(&path, 900.0, UsdImportOptions::default()).unwrap();
    let warning = outside_range(&scene).expect("time.outside_range");
    assert_eq!(warning.count, 1);
    assert!(
        warning.message.contains("[1001, 1050]"),
        "{}",
        warning.message
    );
}

/// `0..0`, what the index gives a stage that authors no range, is no range
/// (spec "No range authored").
#[test]
fn no_authored_range_raises_nothing() {
    let path = write("no_range.usda", &format!("#usda 1.0\n{CAMERA}"));
    let scene = commit(&path, 900.0, UsdImportOptions::default()).unwrap();
    assert!(outside_range(&scene).is_none(), "{:#?}", scene.warnings);
}

/// A pass that is not a `RenderPass` is explained from the index (spec "A
/// pass that is a mesh").
#[test]
fn a_pass_naming_a_mesh_says_so_without_host_facts() {
    let path = write(
        "pass_mesh.usda",
        r#"#usda 1.0
def Mesh "ground"
{
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
}
"#,
    );
    let options = UsdImportOptions {
        pass: Some("/ground".to_owned()),
        ..UsdImportOptions::default()
    };
    let error = commit(&path, 1.0, options)
        .err()
        .expect("the pass is refused");
    let text = error.to_string();
    assert!(
        text.contains("/ground") && text.contains("a Mesh is not a RenderPass"),
        "{text}"
    );

    let missing = UsdImportOptions {
        pass: Some("/nowhere".to_owned()),
        ..UsdImportOptions::default()
    };
    let text = commit(&path, 1.0, missing)
        .err()
        .expect("refused")
        .to_string();
    assert!(text.contains("no prim at this path"), "{text}");
}
