//! The render delegate a long-lived host holds (`delegate` feature): render
//! control around the scene delegate, driven as a Hydra host drives one —
//! the host opens the stage, keeps its stage scene index and Moho's standard
//! chain, edits the stage, applies the index's pending updates and calls
//! `update`.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crust_core::{RenderControl, RenderOutcome, Renderer, Scene, StageHostFacts, UpdateClass};
use crust_render::Assets;
use crust_render::delegate::{Delegate, Error, Hooks, Pick, RenderHandle};
use moho::openusd::usd::{InitialLoadSet, Stage};
use moho::stage::StageSceneIndex;

const STAGE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)
def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 24
        float horizontalAperture = 24
        float verticalAperture = 13.5
        double3 xformOp:translate = (0, 1, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Mesh "A" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, -2, -1), (4, -2, -1), (4, 4, -1), (-4, 4, -1)]
        rel material:binding = </World/Looks/Red>
    }
    def Scope "Looks"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </World/Looks/Red/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }
        }
    }
    def SphereLight "Key"
    {
        float inputs:radius = 0.3
        float inputs:intensity = 30
        double3 xformOp:translate = (0, 3, 2)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        int2 resolution = (32, 18)
        int crust:samplesPerPixel = 16
    }
}
"#;

fn write(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_render_delegate");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, STAGE).unwrap();
    path
}

/// A host: the stage it opened, its stage scene index, and the delegate over
/// Moho's standard chain.
struct Host {
    stage: Stage,
    index: Rc<StageSceneIndex>,
    facts: StageHostFacts,
    delegate: Delegate,
}

impl Host {
    fn open(path: &Path) -> Host {
        Host::open_with(path, None)
    }

    /// [`Host::open`], the delegate loading through `assets` when given.
    fn open_with(path: &Path, assets: Option<Arc<dyn Assets>>) -> Host {
        let stage = Stage::builder()
            .schema_registry(crust_core::usd_schema_registry())
            .load(InitialLoadSet::LoadAll)
            .open(path.to_str().unwrap())
            .unwrap();
        let index = StageSceneIndex::new();
        index.set_stage(stage.clone());
        let standard = moho::chain::renderer_filters(
            moho::chain::standard(index.clone()),
            &[moho::chain::RendererFilter::scene_globals(|_| {})],
        );
        let facts = StageHostFacts::new(&stage, path, Some(standard.clone()));
        let mut delegate = match assets {
            Some(assets) => Delegate::with_assets(standard, path, Hooks::default(), assets),
            None => Delegate::new(standard, path, Hooks::default()),
        };
        assert_eq!(delegate.update(&facts).unwrap(), UpdateClass::Full);
        Host {
            stage,
            index,
            facts,
            delegate,
        }
    }

    fn update(&mut self) -> UpdateClass {
        self.index.apply_pending_updates();
        self.delegate.update(&self.facts).unwrap()
    }

    /// Waits for the running render to return.
    fn wait(&self) {
        let started = Instant::now();
        while self.delegate.outcome().is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the render returns"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// render-delegate spec, "A host-driven edit": the next render uses the new
/// intensity, and the host opened no file for it.
#[test]
fn a_host_driven_edit_restarts_the_render_with_it() {
    let path = write("edit.usda");
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    host.wait();
    let before = host.delegate.snapshot().unwrap();
    assert!(before.completed);
    host.stage
        .attribute("/World/Key.inputs:intensity")
        .unwrap()
        .set(90.0f32)
        .unwrap();
    assert_eq!(host.update(), UpdateClass::Lights);
    assert_eq!(host.delegate.render(), before.render + 1, "a restart");
    host.wait();
    let after = host.delegate.snapshot().unwrap();
    assert!(after.generation > before.generation);
    let mean = |s: &crust_render::delegate::Snapshot| s.rgba.iter().sum::<f32>();
    assert!(mean(&after) > mean(&before), "the brighter light renders");
}

/// An update that changes nothing does not restart the render.
#[test]
fn an_unchanged_update_keeps_the_render() {
    let path = write("unchanged.usda");
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    let render = host.delegate.render();
    assert_eq!(host.update(), UpdateClass::Unchanged);
    assert_eq!(host.delegate.render(), render);
}

/// An update that changes nothing leaves a progressive render running: an
/// empty batch, and one whose only notice is an attribute crust reads
/// nowhere (Moho's generic data). The render is neither cancelled nor
/// restarted.
#[test]
fn an_unchanged_update_leaves_the_render_running() {
    let path = write("unchanged_running.usda");
    let mut host = Host::open(&path);
    host.delegate.start(Some(1 << 16), None).unwrap();
    let render = host.delegate.render();
    assert_eq!(host.update(), UpdateClass::Unchanged);
    host.stage
        .prim("/World/A")
        .unwrap()
        .create_attribute("userProperties:note", "string")
        .unwrap()
        .set("unread".to_string())
        .unwrap();
    assert_eq!(host.update(), UpdateClass::Unchanged);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(host.delegate.render(), render, "not restarted");
    assert_eq!(host.delegate.outcome(), None, "still running");
    host.delegate.cancel();
    assert_eq!(host.delegate.outcome(), Some(RenderOutcome::Cancelled));
}

/// A host thread hook that panics ends that render as panicked: the host's
/// polls report it, instead of a render that never reports at all.
#[test]
fn a_panicking_thread_hook_is_a_panicked_render() {
    let path = write("panicking_hook.usda");
    let stage = Stage::builder()
        .schema_registry(crust_core::usd_schema_registry())
        .load(InitialLoadSet::LoadAll)
        .open(path.to_str().unwrap())
        .unwrap();
    let index = StageSceneIndex::new();
    index.set_stage(stage.clone());
    let standard = moho::chain::renderer_filters(
        moho::chain::standard(index.clone()),
        &[moho::chain::RendererFilter::scene_globals(|_| {})],
    );
    let facts = StageHostFacts::new(&stage, &path, Some(standard.clone()));
    let hooks = Hooks {
        on_thread: Some(Arc::new(|| panic!("the host's hook"))),
        ..Hooks::default()
    };
    let mut delegate = Delegate::new(standard, &path, hooks);
    delegate.update(&facts).unwrap();
    delegate.start(None, None).unwrap();
    let started = Instant::now();
    while !delegate.converged() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the render reports"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let Err(err) = delegate.snapshot() else {
        panic!("a panicked render has no snapshot");
    };
    assert!(format!("{err}").contains("panicked"), "{err}");
}

/// `cancel_render` signals the render to stop without waiting for it.
#[test]
fn cancel_render_returns_at_once() {
    let path = write("cancel.usda");
    let mut host = Host::open(&path);
    host.delegate.start(Some(1 << 16), None).unwrap();
    let handle = host.delegate.handle();
    let asked = Instant::now();
    handle.cancel_render();
    assert!(asked.elapsed() < Duration::from_millis(50), "one store");
    host.wait();
    assert_eq!(host.delegate.outcome(), Some(RenderOutcome::Cancelled));
}

/// A pick names the prim and the material under a pixel.
#[test]
fn a_pick_names_the_hit_prim_and_material() {
    let path = write("pick.usda");
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    let pick = host.delegate.pick(16, 9).unwrap().expect("the wall");
    assert_eq!(pick.prim.as_deref(), Some("/World/A"));
    assert_eq!(pick.material.as_deref(), Some("/World/Looks/Red"));
    assert!(host.delegate.pick(100, 100).is_err(), "outside the image");
}

/// A pick through the render handle answers from another thread while the
/// render runs, and holding it does not keep the next update from applying
/// in place.
#[test]
fn a_pick_from_another_thread_leaves_updates_in_place() {
    let path = write("pick_thread.usda");
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    let handle = host.delegate.handle();
    let picked = std::thread::spawn(move || handle.pick(16, 9))
        .join()
        .unwrap()
        .unwrap()
        .expect("the wall");
    assert_eq!(picked.prim.as_deref(), Some("/World/A"));
    assert_eq!(picked.render, host.delegate.render());
    host.stage
        .attribute("/World/Key.inputs:intensity")
        .unwrap()
        .set(90.0f32)
        .unwrap();
    assert_eq!(
        host.update(),
        UpdateClass::Lights,
        "the scene was taken back whole"
    );
    let handle = host.delegate.handle();
    let render = host.delegate.render();
    let again = std::thread::spawn(move || handle.pick(16, 9))
        .join()
        .unwrap()
        .unwrap()
        .expect("the wall");
    assert_eq!(again.render, render, "the pick names the new render");
}

/// Picks from another thread from inside an update: the first thing an
/// update does is revalidate its assets, so a pick made there is one made
/// while the update runs.
struct PickingAssets {
    files: crust_assets::FileAssets,
    handle: std::sync::Mutex<Option<RenderHandle>>,
    picks: std::sync::Mutex<Vec<Result<Option<Pick>, Error>>>,
}

impl crust_core::AssetLoader for PickingAssets {
    fn load_environment(
        &self,
        path: &Path,
        space: crust_core::ColorSpace,
    ) -> Option<crust_core::EnvironmentMap> {
        self.files.load_environment(path, space)
    }
}

impl Assets for PickingAssets {
    fn preloaded(&self) -> u64 {
        0
    }
    fn revalidate(&self) -> usize {
        if let Some(handle) = self.handle.lock().unwrap().clone() {
            let pick = std::thread::spawn(move || handle.pick(16, 9))
                .join()
                .unwrap();
            self.picks.lock().unwrap().push(pick);
        }
        0
    }
}

/// render-delegate spec, "The host drives and polls the render": a pick
/// from another thread while an update runs is `Busy`, not a hit in the
/// scene the update is replacing; once an update that changes nothing
/// returns, picks answer again.
#[test]
fn a_pick_during_an_update_is_busy() {
    let path = write("pick_during_update.usda");
    let assets = Arc::new(PickingAssets {
        files: crust_assets::FileAssets::new(),
        handle: std::sync::Mutex::new(None),
        picks: std::sync::Mutex::new(Vec::new()),
    });
    let mut host = Host::open_with(&path, Some(assets.clone()));
    host.delegate.start(None, None).unwrap();
    assert!(host.delegate.pick(16, 9).unwrap().is_some(), "the wall");
    *assets.handle.lock().unwrap() = Some(host.delegate.handle());
    host.stage
        .attribute("/World/Key.inputs:intensity")
        .unwrap()
        .set(90.0f32)
        .unwrap();
    assert_eq!(host.update(), UpdateClass::Lights);
    assert_eq!(host.update(), UpdateClass::Unchanged);
    let picks = std::mem::take(&mut *assets.picks.lock().unwrap());
    assert_eq!(picks.len(), 2);
    for pick in picks {
        assert!(matches!(pick, Err(Error::Busy(_))), "{pick:?}");
    }
    let render = host.delegate.render();
    let after = host.delegate.pick(16, 9).unwrap().expect("the wall");
    assert_eq!(after.render, render, "picks answer after the update");
}

/// The delegate's final image is `crust render`'s, bit for bit.
#[test]
fn the_delegate_renders_what_crust_render_renders() {
    let path = write("agree.usda");
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    host.wait();
    let snapshot = host.delegate.snapshot().unwrap();
    let scene = Scene::from_usd(&path).unwrap();
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, scene.settings);
    let rendered = renderer.render_with_control(true, None, None, &RenderControl::new());
    let (w, h) = rendered.buffer.size();
    assert_eq!((w, h), (snapshot.width, snapshot.height));
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = rendered.buffer.get_rgb(x, y);
            let i = (y * w + x) * 4;
            assert_eq!(
                [r, g, b].map(f32::to_bits),
                [snapshot.rgba[i], snapshot.rgba[i + 1], snapshot.rgba[i + 2]].map(f32::to_bits),
                "({x}, {y})"
            );
        }
    }
}

/// render-delegate spec, kept assets: a texture rewritten on disk sends the
/// index no notice, so the next update commits afresh and decodes it again,
/// and renders what `crust render` renders from the new file.
#[test]
fn a_texture_changed_on_disk_is_decoded_again() {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/textures");
    let dir = std::env::temp_dir().join("crust_render_delegate_texture");
    std::fs::create_dir_all(&dir).unwrap();
    let texture = dir.join("albedo.png");
    std::fs::copy(samples.join("mtlx_base.1001.png"), &texture).unwrap();
    let stage = STAGE
        .replace(
            "color3f inputs:diffuseColor = (0.8, 0.1, 0.1)",
            "color3f inputs:diffuseColor.connect = </World/Looks/Red/T.outputs:rgb>",
        )
        .replace(
            "                token outputs:surface\n            }\n",
            &format!(
                "                token outputs:surface\n            }}\n            def Shader \"T\"\n            {{\n                uniform token info:id = \"UsdUVTexture\"\n                asset inputs:file = @{}@\n                vector3f outputs:rgb\n            }}\n",
                texture.display()
            ),
        )
        .replace(
            "rel material:binding = </World/Looks/Red>",
            "rel material:binding = </World/Looks/Red>\n        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = \"vertex\")",
        );
    assert!(stage.contains("UsdUVTexture") && stage.contains("primvars:st"));
    let path = dir.join("textured.usda");
    std::fs::write(&path, &stage).unwrap();
    let mut host = Host::open(&path);
    host.delegate.start(None, None).unwrap();
    host.wait();
    let before = host.delegate.snapshot().unwrap();

    let other = samples.join("mtlx_base.1002.png");
    assert_ne!(
        std::fs::metadata(&other).unwrap().len(),
        std::fs::metadata(&texture).unwrap().len()
    );
    std::fs::copy(&other, &texture).unwrap();
    assert_eq!(host.update(), UpdateClass::Full, "committed afresh");
    host.wait();
    let after = host.delegate.snapshot().unwrap();
    assert_ne!(before.rgba, after.rgba, "the new file renders");

    let scene = Scene::from_usd_with_assets(&path, &crust_assets::FileAssets::new()).unwrap();
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, scene.settings);
    let rendered = renderer.render_with_control(true, None, None, &RenderControl::new());
    let (w, h) = rendered.buffer.size();
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = rendered.buffer.get_rgb(x, y);
            let i = (y * w + x) * 4;
            assert_eq!(
                [r, g, b].map(f32::to_bits),
                [after.rgba[i], after.rgba[i + 1], after.rgba[i + 2]].map(f32::to_bits),
                "({x}, {y})"
            );
        }
    }
}
