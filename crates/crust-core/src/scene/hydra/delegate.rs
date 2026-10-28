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

/// How an update brought the scene up to date (design D9), cheapest first.
pub use super::commit::Class as UpdateClass;

/// Crust's render delegate over a host's index, kept across updates
/// (design D1, D9): a [`HydraScene`], committed into a [`crate::Scene`] at
/// its first update and brought up to date at each later one, in place when
/// the batch allows it. It owns no stage, opens no file and reads no layer:
/// the host edits its stage, sets its time, applies its stage scene index's
/// pending updates and calls [`SceneDelegate::update`].
pub struct SceneDelegate {
    hydra: HydraScene,
    options: crate::UsdImportOptions,
    assets: std::sync::Arc<dyn crate::AssetLoader>,
    /// The stage's file: where an asset path openusd handed back unresolved
    /// is anchored.
    path: std::path::PathBuf,
}

/// What [`SceneDelegate::update`] gives back.
pub struct Updated {
    pub scene: crate::Scene,
    pub class: UpdateClass,
    /// How long flushing the batched notices took (Moho's side of the
    /// update); the rest is the commit's.
    pub flush: std::time::Duration,
}

impl SceneDelegate {
    /// A delegate over `standard`, the host's `moho::chain::standard` over
    /// its stage scene index, whose stage was opened from `path`, importing
    /// with `options` (its `frame` is the index's `sceneGlobals/currentFrame`
    /// at each update) through
    /// `assets`.
    pub fn new(
        standard: SceneIndexHandle,
        path: &std::path::Path,
        options: crate::UsdImportOptions,
        assets: std::sync::Arc<dyn crate::AssetLoader>,
    ) -> Self {
        let options = crate::UsdImportOptions {
            // Geometry edits apply in place only over kept geometry.
            keep_geometry: crate::config().scene_deltas,
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

    /// Flushes the index's batched notices and brings `scene` (the one the
    /// last update gave back, `None` at the first) up to date with them, as
    /// D9's class table decides: in place when the batch allows it, else by
    /// committing afresh from the managed prims. Either way the scene is the
    /// one a fresh delegate over the same index commits, bit for bit.
    pub fn update(
        &mut self,
        facts: &dyn super::index::host::HostFacts,
        scene: Option<crate::Scene>,
    ) -> Result<Updated, crate::Error> {
        let flushing = std::time::Instant::now();
        let batch = self.hydra.update();
        let flush = flushing.elapsed();
        let mut options = self.options.clone();
        options.frame = super::index::data::current_frame(self.hydra.index().as_ref());
        let (class, change) =
            super::commit::classify(&batch, scene.as_ref().and_then(|s| s.provenance.as_ref()));
        let Some(mut scene) = scene else {
            return Ok(Updated {
                scene: self.commit(facts, &options)?,
                class: UpdateClass::Full,
                flush,
            });
        };
        match class {
            UpdateClass::Unchanged => {
                return Ok(Updated {
                    scene,
                    class,
                    flush,
                });
            }
            UpdateClass::Full => {
                return Ok(Updated {
                    scene: self.commit(facts, &options)?,
                    class,
                    flush,
                });
            }
            _ if change.recommit => {
                tracing::debug!(
                    "{} update: committed from the managed prims",
                    class.as_str()
                );
                return Ok(Updated {
                    scene: self.commit(facts, &options)?,
                    class,
                    flush,
                });
            }
            _ => {}
        }
        let at = super::update::Rebuild {
            index: self.hydra.index(),
            facts,
            path: &self.path,
            assets: &*self.assets,
        };
        let started = std::time::Instant::now();
        let applied = match scene.provenance.as_ref() {
            Some(prov) => super::update::rebuild(prov, &at, &scene.warnings, &change, &scene.world)
                .and_then(|update| update.apply(&mut scene)),
            None => Err(crate::live::Structural(
                "the scene has no provenance".into(),
            )),
        };
        match applied {
            Ok(()) => {
                scene.time = options.frame;
                if let Some(prov) = scene.provenance.as_mut() {
                    prov.stage.options.frame = options.frame;
                }
                scene.stats.record(
                    format!("Update in place ({})", class.as_str()),
                    0,
                    started.elapsed(),
                );
                Ok(Updated {
                    scene,
                    class,
                    flush,
                })
            }
            Err(why) => {
                tracing::debug!("{} update committed afresh: {}", class.as_str(), why.0);
                Ok(Updated {
                    scene: self.commit(facts, &options)?,
                    class: UpdateClass::Full,
                    flush,
                })
            }
        }
    }

    /// The scene committed afresh from the managed prims.
    fn commit(
        &self,
        facts: &dyn super::index::host::HostFacts,
        options: &crate::UsdImportOptions,
    ) -> Result<crate::Scene, crate::Error> {
        let valid = super::ValidOptions::check(options)?;
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
            options,
            valid,
        )
    }
}
