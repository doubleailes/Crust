//! A host of Crust's render delegate, for the tests that drive one: a stage
//! it opens and edits, the stage scene index over it, Moho's standard chain
//! and a [`SceneDelegate`] over that, as a Hydra host holds them.
#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crust_core::{Renderer, Scene, SceneDelegate, StageHostFacts, UpdateClass, UsdImportOptions};
use moho::chain::RendererFilter;
use moho::filter::SceneGlobalsSceneIndex;
use moho::openusd::sdf;
use moho::openusd::usd::{InitialLoadSet, Stage, TimeCode};
use moho::scene_index::SceneIndexHandle;
use moho::stage::StageSceneIndex;
use std::cell::OnceCell;

/// A stage written from `text` to a temporary file named `name`.
pub fn write(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_delegate_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// A host over the stage at `path`.
pub struct Host {
    pub path: PathBuf,
    pub stage: Stage,
    index: Rc<StageSceneIndex>,
    globals: Rc<SceneGlobalsSceneIndex>,
    facts: StageHostFacts,
    delegate: SceneDelegate,
    scene: Option<Scene>,
    time: Option<f64>,
}

fn open(path: &Path) -> Stage {
    Stage::builder()
        .schema_registry(crust_core::usd_schema_registry())
        .load(InitialLoadSet::LoadAll)
        .open(path.to_str().unwrap())
        .unwrap()
}

/// What a host builds over a stage: its stage scene index, the scene globals
/// filter it appends to Moho's standard chain (as usdview registers
/// `HdsiSceneGlobalsSceneIndex`), the stage's facts, the delegate and its
/// first scene.
type Built = (
    Rc<StageSceneIndex>,
    Rc<SceneGlobalsSceneIndex>,
    StageHostFacts,
    SceneDelegate,
    Scene,
);

/// What a test puts between the host's chain and the delegate.
pub type Wrap = Box<dyn Fn(SceneIndexHandle) -> SceneIndexHandle>;

/// The delegate over a stage scene index of `stage` at `time`, updated once,
/// reading the chain through `wrap`.
fn delegate_over(
    stage: &Stage,
    path: &Path,
    time: Option<f64>,
    options: UsdImportOptions,
    wrap: Option<Wrap>,
    assets: Arc<dyn crust_core::AssetLoader>,
) -> Built {
    let index = StageSceneIndex::new();
    if let Some(t) = time {
        index.set_time(TimeCode::new(t));
    }
    // The stage, then the chain: either order reads the root's data, since
    // the stage index adds `/` when it populates, as
    // `UsdImagingStageSceneIndex` does (Moho PR #25).
    index.set_stage(stage.clone());
    let cell: Rc<OnceCell<Rc<SceneGlobalsSceneIndex>>> = Rc::default();
    let captured = cell.clone();
    let standard = moho::chain::renderer_filters(
        moho::chain::standard(index.clone()),
        &[RendererFilter::scene_globals(move |g| {
            let _ = captured.set(g);
        })],
    );
    let globals = cell.get().unwrap().clone();
    if let Some(t) = time {
        globals.set_current_frame(t);
    }
    let facts = StageHostFacts::new(stage);
    let read = match wrap {
        Some(wrap) => wrap(standard),
        None => standard,
    };
    let mut delegate = SceneDelegate::new(read, path, options, assets);
    let updated = delegate.update(&facts, None).unwrap();
    assert_eq!(updated.class, UpdateClass::Full, "a first update commits");
    (index, globals, facts, delegate, updated.scene)
}

impl Host {
    /// Opens the stage at `path` and commits it.
    pub fn open(path: &Path) -> Host {
        Host::open_with(path, UsdImportOptions::default())
    }

    /// [`Host::open`] with the delegate importing under `options`.
    pub fn open_with(path: &Path, options: UsdImportOptions) -> Host {
        Host::open_wrapped(path, options, None)
    }

    /// [`Host::open_with`], the delegate reading the chain through `wrap`.
    pub fn open_wrapped(path: &Path, options: UsdImportOptions, wrap: Option<Wrap>) -> Host {
        Host::open_loading(path, options, wrap, Arc::new(crust_core::NoAssets))
    }

    /// [`Host::open_wrapped`], the delegate loading assets through `assets`.
    pub fn open_loading(
        path: &Path,
        options: UsdImportOptions,
        wrap: Option<Wrap>,
        assets: Arc<dyn crust_core::AssetLoader>,
    ) -> Host {
        let stage = open(path);
        let (index, globals, facts, delegate, scene) =
            delegate_over(&stage, path, None, options, wrap, assets);
        Host {
            path: path.to_path_buf(),
            stage,
            index,
            globals,
            facts,
            delegate,
            scene: Some(scene),
            time: None,
        }
    }

    /// Sets the value of the attribute at `attr` (`/Prim.name`).
    pub fn set(&self, attr: &str, value: impl Into<sdf::Value>) {
        self.stage.attribute(attr).unwrap().set(value).unwrap();
    }

    /// Moves the stage scene index's time.
    pub fn set_time(&mut self, time: f64) {
        self.time = Some(time);
        self.index.set_time(TimeCode::new(time));
        self.globals.set_current_frame(time);
    }

    /// Applies the index's pending updates and asks the delegate what the
    /// batch would do to the scene it holds (`None` when an update failed).
    pub fn prepare(&mut self) -> UpdateClass {
        self.index.apply_pending_updates();
        let provenance = self.scene.as_ref().and_then(|s| s.provenance.as_ref());
        self.delegate.prepare(provenance)
    }

    /// [`Host::update`], keeping a failure: the scene is then gone, as a
    /// failed update consumed it.
    pub fn try_update(&mut self) -> Result<UpdateClass, crust_core::Error> {
        self.index.apply_pending_updates();
        let updated = self.delegate.update(&self.facts, self.scene.take())?;
        self.scene = Some(updated.scene);
        Ok(updated.class)
    }

    /// Applies the index's pending updates and updates the delegate.
    pub fn update(&mut self) -> UpdateClass {
        self.index.apply_pending_updates();
        let updated = self
            .delegate
            .update(&self.facts, self.scene.take())
            .unwrap();
        self.scene = Some(updated.scene);
        updated.class
    }

    pub fn scene(&self) -> &Scene {
        self.scene.as_ref().unwrap()
    }

    pub fn take(&mut self) -> Scene {
        self.scene.take().unwrap()
    }

    /// A fresh delegate's scene over the same stage, as it is now.
    pub fn fresh(&self) -> Scene {
        delegate_over(
            &self.stage,
            &self.path,
            self.time,
            UsdImportOptions::default(),
            None,
            Arc::new(crust_core::NoAssets),
        )
        .4
    }
}

/// The scene rendered at 16 spp, as bits.
pub fn bits(scene: Scene) -> Vec<u32> {
    let settings = scene.settings.with_samples_per_pixel(16);
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
