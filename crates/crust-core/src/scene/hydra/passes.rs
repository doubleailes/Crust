//! The active `UsdRenderPass`: its `matte` collection's geometry is held out,
//! and what its `cameraVisibility` collection leaves out is hidden from
//! camera rays.
//!
//! The host names the pass ([`crate::UsdImportOptions::pass`]); a stage that
//! defines passes renders with none unless one is named, as Hydra leaves the
//! active pass to the application. Its `renderSource` is read by
//! [`render_settings_path`](super::settings::render_settings_path), not here.
//!
//! Membership is Hydra's: the pass prim's
//! `collections/<name>/membershipExpression`, the collection's complete
//! membership as UsdImaging computes it, judged with Hydra's predicates over
//! the scene index on the path of the prim that brings the geometry in — the
//! instance or instancer prim, never a prototype's. A collection restricts
//! anything when its expression differs from its fallback: nothing for
//! `matte` and `prune`, everything (`~//*.*`, `includeRoot` 1) for
//! `cameraVisibility` and `renderVisibility`.
//!
//! Each answer is written straight into the world: a holdout flag
//! ([`WorldBuilder::set_holdout`]), and the camera bit cleared from the mask
//! (never set — a pass cannot show the camera what `crust:rayMask` or the
//! hidden-light rule hides). A light at infinity loses camera visibility from
//! its own escape mask. All of it lands before [`super::light_links`]
//! resolves, so a light linked to nothing is demoted on the pass's camera
//! visibility.
//!
//! `prune` and `renderVisibility` are not read: they change what the
//! traversal emits ([`ActivePass::read`] warns when they restrict anything).

use crate::warning;
use std::ops::Range;

use moho::collection::CollectionExpressionEvaluator;
use moho::openusd::sdf;
use moho::openusd::sdf::PathExpression;
use moho::scene_index::{SceneIndex, SceneIndexHandle};
use moho::tokens::collections as coll;
use moho::tokens::usd_prim_info as prim_info;
use tracing::debug;

use crate::light::{Light, LightList};

// A `RenderPass`'s collections (`UsdRenderPass`'s schema). openusd-schemas
// dropped its constants for them at d7d2a29, keeping only the
// `renderVisibility` view (`Pass::render_visibility_collection`), so the
// names are spelled here.
const COLLECTION_CAMERA_VISIBILITY: &str = "cameraVisibility";
const COLLECTION_PRUNE: &str = "prune";
const COLLECTION_MATTE: &str = "matte";
use crate::ray::{MASK_CAMERA, RayMask};
use crate::rt_world::WorldBuilder;

use super::index::Input;
use super::index::data::PrimData;

/// `renderVisibility`, which `openusd-schemas` names no constant for.
const COLLECTION_RENDER_VISIBILITY: &str = "renderVisibility";

/// What the active pass did, for `crust check`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassSummary {
    /// The `RenderPass` prim.
    pub path: String,
    /// How many prims that emit geometry are holdouts.
    pub holdouts: usize,
    /// How many prims that emit geometry the pass hides from the camera.
    pub camera_hidden: usize,
}

/// The membership expression of collection `name` on the pass `prim`, as
/// Hydra gives it.
fn expression(prim: &PrimData, name: &str) -> Option<PathExpression> {
    match prim
        .locate([coll::COLLECTIONS, name, coll::MEMBERSHIP_EXPRESSION])?
        .value()?
    {
        sdf::Value::PathExpression(expression) => Some(expression),
        _ => None,
    }
}

/// Whether `expression` says anything beyond its collection's fallback:
/// nothing when `includes_root` is false (`matte`, `prune`), everything but
/// properties when it is true (`includeRoot` 1, which `RenderPass` declares
/// for `cameraVisibility` and `renderVisibility`).
fn restricts(expression: &PathExpression, includes_root: bool) -> bool {
    if includes_root {
        *expression != PathExpression::parse("~//*.*")
    } else {
        !expression.is_empty()
    }
}

/// The paths `expression` names.
fn named_paths(expression: &PathExpression) -> Vec<sdf::Path> {
    let mut paths = Vec::new();
    let _ = expression.clone().map_paths(|p| {
        paths.push(p.clone());
        Some(p.clone())
    });
    paths
}

/// The active pass, through the traversal.
pub(in crate::scene) struct ActivePass {
    path: sdf::Path,
    /// For each collection that restricts anything (`None` for one that
    /// does not), its complete membership expression, as Hydra's
    /// `collections/<name>/membershipExpression` gives it.
    queries: [Option<CollectionExpressionEvaluator>; 2],
    /// The scene index the expressions are judged over.
    index: Option<SceneIndexHandle>,
    holdouts: usize,
    camera_hidden: usize,
}

impl ActivePass {
    /// The pass at `path`, which [`super::settings::check_pass`] has found
    /// to be a `RenderPass`. Warns once per collection it does not read that
    /// restricts anything (a pass's `prune` and `renderVisibility`).
    pub(in crate::scene) fn read(scene: &dyn SceneIndex, path: &sdf::Path) -> Self {
        let prim = PrimData::at(scene, path);
        for (name, includes_root) in [
            (COLLECTION_PRUNE, false),
            (COLLECTION_RENDER_VISIBILITY, true),
        ] {
            if expression(&prim, name).is_some_and(|e| restricts(&e, includes_root)) {
                warning!(
                    RenderPassCollectionNotRead,
                    at = path,
                    "{path}: collection:{name} restricts what renders, which crust does not \
                     read — rendered as if it were unauthored"
                );
            }
        }
        ActivePass {
            path: path.clone(),
            queries: [None, None],
            index: None,
            holdouts: 0,
            camera_hidden: 0,
        }
    }

    /// Builds the queries over `input`, which the commit reads, for each
    /// collection that restricts anything. Warns once per collection that
    /// names a prim inside an instance: membership is judged on the
    /// instance, so that prim matches nothing.
    pub(in crate::scene) fn begin(&mut self, input: &Input) {
        self.queries = [None, None];
        self.index = Some(input.chain.clone());
        let index: &dyn SceneIndex = input.chain.as_ref();
        let prim = PrimData::at(index, &self.path);
        if !prim.is_defined() {
            debug!("Render pass {} is not on the stage", self.path);
            return;
        }
        let path = &self.path;
        for (k, (name, includes_root)) in [
            (COLLECTION_MATTE, false),
            (COLLECTION_CAMERA_VISIBILITY, true),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(expression) = expression(&prim, name) else {
                debug!("{path}: collection:{name} unreadable — read as its fallback");
                continue;
            };
            if !restricts(&expression, includes_root) {
                continue;
            }
            if let Some(t) = named_paths(&expression)
                .into_iter()
                .find(|t| inside_instance(index, t))
            {
                warning!(
                    RenderPassTargetInInstance,
                    at = path,
                    "{path}: collection:{name} names {t}, inside an instance — membership is \
                     judged on the instance, so it matches no instance"
                );
            }
            // Hydra's complete membership expression, judged with Hydra's
            // predicates over the scene index.
            self.queries[k] = Some(CollectionExpressionEvaluator::new(&expression));
        }
    }

    /// Whether the prim at `path` is held out, and whether it is hidden from
    /// the camera, by the queries.
    fn judge(&self, path: &sdf::Path) -> (bool, bool) {
        let [matte, camera] = &self.queries;
        let Some(index) = &self.index else {
            return (false, false);
        };
        let includes = |q: &CollectionExpressionEvaluator| q.matches(index.as_ref(), path);
        (
            matte.as_ref().is_some_and(includes),
            camera.as_ref().is_some_and(|q| !includes(q)),
        )
    }

    /// The prim at `path` emitted the geometries `geoms` and the volume
    /// regions `vols`: flags them as the pass says.
    pub(in crate::scene) fn saw(
        &mut self,
        path: &sdf::Path,
        geoms: Range<usize>,
        vols: Range<usize>,
        world: &mut WorldBuilder,
    ) {
        if self.queries.iter().all(Option::is_none) || (geoms.is_empty() && vols.is_empty()) {
            return;
        }
        let (held, hidden) = self.judge(path);
        if !geoms.is_empty() {
            for g in geoms.start as u32..geoms.end as u32 {
                if held {
                    world.set_holdout(g, true);
                }
                if hidden {
                    let mask = world.mask(g);
                    world.set_mask(g, RayMask(mask.0 & !MASK_CAMERA.0));
                }
            }
            self.holdouts += held as usize;
            self.camera_hidden += hidden as usize;
        }
        if !vols.is_empty() && (held || hidden) {
            let (pass, why) = (&self.path, if held { "in" } else { "outside" });
            let name = if held { "matte" } else { "cameraVisibility" };
            warning!(
                RenderPassVolumeIgnored,
                at = path,
                "{path}: a volume region {why} collection:{name} of render pass {pass}, which \
                 crust does not apply to volumes — the camera sees it as without the pass"
            );
        }
    }

    /// The light at `path` was just added at `index` of `lights`. A light at
    /// infinity held out or hidden from the camera is invisible to escaping
    /// camera rays: zero colour and zero alpha is what both would give. A
    /// finite light's geometry was flagged by [`ActivePass::saw`].
    pub(in crate::scene) fn light_added(
        &self,
        path: &sdf::Path,
        index: usize,
        lights: &mut LightList,
    ) {
        if self.queries.iter().all(Option::is_none) || !lights.light(index).at_infinity() {
            return;
        }
        let (held, hidden) = self.judge(path);
        if held || hidden {
            debug!(
                "{path}: hidden from the camera by render pass {}",
                self.path
            );
            lights.hide_from_camera(index);
        }
    }

    pub(in crate::scene) fn summary(&self) -> PassSummary {
        PassSummary {
            path: self.path.to_string(),
            holdouts: self.holdouts,
            camera_hidden: self.camera_hidden,
        }
    }
}

/// Whether some strict ancestor of `path` is a native instance (Moho's
/// `__usdPrimInfo/niPrototypePath`), i.e. `path` names geometry inside a
/// prototype.
fn inside_instance(index: &dyn SceneIndex, path: &sdf::Path) -> bool {
    let mut p = path.parent();
    while let Some(a) = p {
        if a.is_abs_root() {
            return false;
        }
        if PrimData::at(index, &a)
            .locate([prim_info::USD_PRIM_INFO, prim_info::NI_PROTOTYPE_PATH])
            .is_some()
        {
            return true;
        }
        p = a.parent();
    }
    false
}
