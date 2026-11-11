//! `HydraScene`, the delegate's scene side (design D1): the host's index
//! with the delegate's renderer filters, its notices batched by prim type,
//! observed as managed prims. Each update flushes the batch; the commit
//! reads the index in path order ([`super::commit`]).

use std::rc::Rc;

use moho::filter::prim_type_notice_batching::{PrimTypeNoticeBatchingSceneIndex, PrimTypePriority};
use moho::openusd::sdf;
use moho::prim_managing::PrimManagingObserver;
use moho::scene_index::SceneIndexHandle;

use super::factory::{CrustPrimFactory, CrustPriority};
use super::index::Input;
use super::prims::{Batch, SharedBatch};

/// The delegate over a host's index.
pub(in crate::scene) struct HydraScene {
    batching: Rc<PrimTypeNoticeBatchingSceneIndex>,
    observer: Rc<PrimManagingObserver>,
    batch: SharedBatch,
}

impl HydraScene {
    /// A delegate over `standard`, the host's `moho::chain::standard`. Empty
    /// until its first [`update`](Self::update), which adds every prim.
    pub(in crate::scene) fn new(standard: SceneIndexHandle) -> Self {
        Self::with_priority(standard, Rc::new(CrustPriority))
    }

    /// [`new`](Self::new), the notices sent in `priority`'s order rather than
    /// D1's: the commit does not depend on it.
    pub(in crate::scene) fn with_priority(
        standard: SceneIndexHandle,
        priority: Rc<dyn PrimTypePriority>,
    ) -> Self {
        let input = Input::over(standard);
        let batching = PrimTypeNoticeBatchingSceneIndex::new(input.chain, Some(priority));
        let handle: SceneIndexHandle = batching.clone();
        let batch = SharedBatch::default();
        let factory = Rc::new(CrustPrimFactory {
            batch: batch.clone(),
        });
        let observer = PrimManagingObserver::new(handle, Some(factory));
        HydraScene {
            batching,
            observer,
            batch,
        }
    }

    /// Flushes the batching index and returns what it brought: the prims
    /// added and removed, and the locators dirtied.
    pub(in crate::scene) fn update(&self) -> Batch {
        self.batching.flush();
        std::mem::take(&mut *self.batch.borrow_mut())
    }

    /// The index the commit reads: the observed, batched one.
    pub(in crate::scene) fn index(&self) -> SceneIndexHandle {
        self.observer.scene_index().clone()
    }

    /// The managed prims' paths, in path order.
    pub(in crate::scene) fn paths(&self) -> Vec<sdf::Path> {
        self.observer.paths()
    }
}

/// Crust's render delegate over a host's index: a [`HydraScene`], committed
/// into a [`crate::Scene`]. It owns no stage, opens no file and reads no
/// layer: the host opens its stage, sets its time, and calls
/// [`SceneDelegate::commit`]. Each commit reads the whole index afresh.
pub struct SceneDelegate {
    hydra: HydraScene,
    options: crate::UsdImportOptions,
    assets: std::sync::Arc<dyn crate::AssetLoader>,
    /// The stage's file: where an asset path openusd handed back unresolved
    /// is anchored.
    path: std::path::PathBuf,
}

impl SceneDelegate {
    /// A delegate over `standard`, the host's `moho::chain::standard` over
    /// its stage scene index, whose stage was opened from `path`, importing
    /// with `options` (its `frame` is the index's `sceneGlobals/currentFrame`
    /// at each commit) through `assets`.
    pub fn new(
        standard: SceneIndexHandle,
        path: &std::path::Path,
        options: crate::UsdImportOptions,
        assets: std::sync::Arc<dyn crate::AssetLoader>,
    ) -> Self {
        let options = crate::UsdImportOptions {
            skip_stage_teardown: false,
            stage_checks: false,
            ..options
        };
        SceneDelegate {
            hydra: HydraScene::new(standard),
            options,
            assets,
            path: path.to_path_buf(),
        }
    }

    /// Flushes the index's batched notices and commits the scene afresh from
    /// the managed prims, at the index's time.
    pub fn commit(
        &mut self,
        facts: &dyn super::index::host::HostFacts,
    ) -> Result<crate::Scene, crate::Error> {
        self.hydra.update();
        let mut options = self.options.clone();
        options.frame = super::index::data::current_frame(self.hydra.index().as_ref());
        let valid = super::ValidOptions::check(&options)?;
        let now = std::time::Instant::now();
        super::commit_scene(
            super::Holder::Kept(&self.hydra),
            super::Commit {
                facts,
                path: &self.path,
                import_start: now,
                open_start: now,
                checks_elapsed: None,
                warning_scope: crate::WarningScope::enter(),
            },
            &*self.assets,
            &options,
            valid,
        )
    }
}
