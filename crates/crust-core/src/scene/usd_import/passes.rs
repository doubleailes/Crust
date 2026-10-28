//! The active `UsdRenderPass`: its `matte` collection's geometry is held out,
//! and what its `cameraVisibility` collection leaves out is hidden from
//! camera rays.
//!
//! The host names the pass ([`crate::UsdImportOptions::pass`]); a stage that
//! defines passes renders with none unless one is named, as Hydra leaves the
//! active pass to the application. Its `renderSource` is read by
//! [`render_settings_path`](super::settings::render_settings_path), not here.
//!
//! Membership is openusd's (`CollectionAPI::membership_query`), judged where
//! light linking judges it ([`super::light_links`]): on the stage path of the
//! prim that brings the geometry in — the instance or instancer prim, never a
//! prototype's — so the two collection features cannot disagree about what
//! "this prim" is. Unlike a light link, the pass is known before the
//! traversal starts, so it is decided at dispatch: each chunk's queries are
//! built from that chunk's stage ([`ActivePass::begin_chunk`]), which has its
//! payloads loaded, so a `membershipExpression` predicate sees the prims it
//! asks about, and opinions the pass itself takes from a payload count. Which
//! collections restrict anything is decided there too, never on the
//! payload-free index stage. The streaming import masks every chunk to the
//! pass prim too ([`ActivePass::path`]).
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

use std::rc::Rc;

use moho::openusd::sdf;
use moho::openusd::usd::CollectionAPI;
use moho::scene_index::{CollectionMembership, SceneIndex};
use moho::stage::StageSceneIndex;
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
use super::light_links::Opinions;

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

/// One of the two collections the pass reads.
#[derive(Clone, Copy)]
enum Which {
    Matte,
    Camera,
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Matte => COLLECTION_MATTE,
            Which::Camera => COLLECTION_CAMERA_VISIBILITY,
        }
    }

    /// Whether `o` makes the collection say anything beyond its fallback:
    /// `matte` includes nothing by default (`includeRoot` 0), and
    /// `cameraVisibility` everything (`includeRoot` 1, which `RenderPass`
    /// declares).
    fn restricts(self, o: &Opinions) -> bool {
        o.expression
            || match self {
                Which::Matte => !o.includes.is_empty() || o.include_root == Some(true),
                Which::Camera => !o.excludes.is_empty() || o.include_root == Some(false),
            }
    }
}

/// The active pass, through the traversal.
pub(super) struct ActivePass {
    path: sdf::Path,
    /// The chunk being walked's stage index, which answers membership for
    /// each collection that restricts anything there (`None` for one that
    /// does not). Decided per chunk, not on the payload-free scene: a pass can
    /// take its collections' opinions from a payload, which only a chunk's
    /// stage loads.
    queries: [Option<Rc<StageSceneIndex>>; 2],
    /// Per collection, whether a target inside an instance / a nested
    /// collection a chunk does not compose was warned about already.
    warned_instance: [bool; 2],
    warned_nested: [bool; 2],
    holdouts: usize,
    camera_hidden: usize,
}

impl ActivePass {
    /// The pass at `path` on the payload-free scene, which
    /// [`super::settings::check_pass`] has found to be a `RenderPass`. Warns
    /// once per collection it does not read that restricts anything (a pass's
    /// `prune` and `renderVisibility` collections, which Moho lists among its
    /// `collections`, M6; their opinions are read from the generic data).
    pub(super) fn read(scene: &dyn SceneIndex, path: &sdf::Path) -> Self {
        let prim = PrimData::at(scene, path);
        for (name, fallback_root) in [
            (COLLECTION_PRUNE, false),
            (COLLECTION_RENDER_VISIBILITY, true),
        ] {
            let o = opinions(&prim, name);
            let restricts = o.expression
                || if fallback_root {
                    !o.excludes.is_empty() || o.include_root == Some(false)
                } else {
                    !o.includes.is_empty() || o.include_root == Some(true)
                };
            if restricts {
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
            warned_instance: [false; 2],
            warned_nested: [false; 2],
            holdouts: 0,
            camera_hidden: 0,
        }
    }

    /// Builds the queries for the chunk `input` is about to be walked from,
    /// for each collection that restricts anything on its stage.
    pub(super) fn begin_chunk(&mut self, input: &Input) {
        self.queries = [None, None];
        let index: &dyn SceneIndex = input.stage.as_ref();
        let prim = PrimData::at(index, &self.path);
        if !prim.is_defined() {
            debug!(
                "Render pass {} is not composed on this chunk's stage",
                self.path
            );
            return;
        }
        for (k, which) in [Which::Matte, Which::Camera].into_iter().enumerate() {
            if which.restricts(&opinions(&prim, which.name())) {
                self.queries[k] = self.query(input, &prim, which, k);
            }
        }
    }

    fn query(
        &mut self,
        input: &Input,
        prim: &PrimData,
        which: Which,
        k: usize,
    ) -> Option<Rc<StageSceneIndex>> {
        let index: &dyn SceneIndex = input.stage.as_ref();
        let name = which.name();
        let path = &self.path;
        let o = opinions(prim, name);
        if !self.warned_instance[k]
            && let Some(t) = o
                .includes
                .iter()
                .chain(&o.excludes)
                .find(|t| inside_instance(index, t))
        {
            warning!(
                RenderPassTargetInInstance,
                at = path,
                "{path}: collection:{name} targets {t}, inside an instance — membership is \
                 judged on the instance, so the target cannot be told apart from its siblings"
            );
            self.warned_instance[k] = true;
        }
        if !self.warned_nested[k]
            && let Some(t) = o.includes.iter().find(|t| {
                CollectionAPI::instance_at_path(t)
                    .is_some_and(|(owner, _)| !PrimData::at(index, &owner).is_defined())
            })
        {
            warning!(
                RenderPassNestedNotComposed,
                at = path,
                "{path}: collection:{name} includes {t}, which a chunk of the streamed import \
                 does not compose — that nested collection contributes nothing there"
            );
            self.warned_nested[k] = true;
        }
        // The chunk's stage index judges membership through openusd's query,
        // membership expressions and their predicates included.
        if input.stage.includes(path, name, path).is_none() {
            debug!("{path}: collection:{name} unreadable — read as its fallback");
            return None;
        }
        Some(input.stage.clone())
    }

    /// Whether the prim at `path` is held out, and whether it is hidden from
    /// the camera, by the chunk's queries.
    fn judge(&self, path: &sdf::Path) -> (bool, bool) {
        let [matte, camera] = &self.queries;
        let includes =
            |q: &Rc<StageSceneIndex>, which: Which| q.includes(&self.path, which.name(), path);
        (
            matte
                .as_ref()
                .is_some_and(|q| includes(q, Which::Matte) == Some(true)),
            camera
                .as_ref()
                .is_some_and(|q| includes(q, Which::Camera) == Some(false)),
        )
    }

    /// The prim at `path` emitted the geometries `geoms` and the volume
    /// regions `vols`: flags them as the pass says.
    pub(super) fn saw(
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
    pub(super) fn light_added(&self, path: &sdf::Path, index: usize, lights: &mut LightList) {
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

    pub(super) fn summary(&self) -> PassSummary {
        PassSummary {
            path: self.path.to_string(),
            holdouts: self.holdouts,
            camera_hidden: self.camera_hidden,
        }
    }
}

/// The opinions collection `name` authors on `prim`, from the generic data:
/// what decides whether it restricts anything, and what its warnings name.
fn opinions(prim: &PrimData, name: &str) -> Opinions {
    let prop = |suffix: &str| format!("collection:{name}:{suffix}");
    Opinions {
        includes: prim.targets(&prop("includes")),
        excludes: prim.targets(&prop("excludes")),
        include_root: prim.custom_bool(&prop("includeRoot")),
        expansion_rule: prim.custom_token(&prop("expansionRule")),
        expression: prim.value(&prop("membershipExpression")).is_some(),
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
