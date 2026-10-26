//! The import through Moho, the Hydra-2.0-style scene index over openusd
//! (the `moho-import` change): the scene indices the import reads a stage
//! through, and the walk over them ([`walk`]).
//!
//! # Layout (design D4)
//!
//! - **One stage** (fewer than [`MIN_STREAM_CHUNKS`](super::MIN_STREAM_CHUNKS)
//!   subtrees, or `CRUST_STREAM_IMPORT=0`): one [`Input`] over the `LoadAll`
//!   stage, light linking over it with the stage as membership oracle — the
//!   shape of `moho::chain::standard`.
//! - **Streamed**: light linking over a `MergingSceneIndex` (the shape of
//!   `moho::chain::merged`), whose weakest, permanent input is the payload-free
//!   scene, and to which each chunk's masked stage is added in turn, walked and
//!   removed ([`Streamed`]).
//!
//! Crust opens every stage itself, with its own `stage_builder()` (the schema
//! registry, the `mmap` resolver), and hands it to the index. openusd is named
//! through Moho's re-exports here, so the types that cross the Moho boundary
//! are visible as such (design D2).
//!
//! # The bridge (design D6)
//!
//! While a schema family has not moved to the index, its existing reader is
//! handed the `usd::Prim` at the walked path on the stage of the input that
//! holds it ([`bridge`]). The bridge cannot reach a prim that exists only in
//! Moho's namespace (re-rooted prototypes), so until instancing moves each
//! input's chain is `chain::input` without the two instancing filters
//! ([`input_chain`]). The bridge is transitional: it goes with the direct
//! readers it calls, at the end of the change.

use std::rc::Rc;

use moho::filter::{LightLinkingSceneIndex, MaterialNodeResolvingSceneIndex, MergingSceneIndex};
use moho::openusd::sdf;
use moho::openusd::usd::{Stage, TimeCode};
use moho::scene_index::{CollectionMembership, SceneIndexHandle};
use moho::stage::StageSceneIndex;

pub(super) mod data;
pub(super) mod instancers;
pub(super) mod network;
pub(super) mod stage_facts;
pub(super) mod walk;

/// One input of the import: a stage's scene index and the chain over it.
pub(super) struct Input {
    /// The stage index, which owns the stage while the input lives.
    pub(super) stage: Rc<StageSceneIndex>,
    /// `moho::chain::input` over it: the native and point instancing
    /// filters, flattening, material binding resolution and render settings
    /// flattening.
    pub(super) chain: SceneIndexHandle,
}

/// The input over `stage`, at the import's `time` (`None` reads default
/// values: the index's time is then left unset, which is Moho's default time).
/// `include_unloaded` exposes the prims of unloaded payloads, as the
/// payload-free scene input needs to hold each chunk's root.
pub(super) fn input_chain(stage: Stage, time: Option<f64>, include_unloaded: bool) -> Input {
    let index = StageSceneIndex::new();
    index.set_include_unloaded_prims(include_unloaded);
    if let Some(t) = time {
        index.set_time(TimeCode::new(t));
    }
    index.set_stage(stage);
    let chain = moho::chain::input(index.clone());
    Input {
        stage: index,
        chain,
    }
}

/// A single-stage import's index: one input with light linking over it,
/// judged by the stage, and the material-node resolution last
/// (`moho::chain::standard`'s shape).
pub(super) struct Single {
    pub(super) input: Input,
    /// The top of the chain: what the material readers read, and every
    /// reader once the families move.
    pub(super) top: SceneIndexHandle,
}

impl Single {
    pub(super) fn new(stage: Stage, time: Option<f64>) -> Self {
        let input = input_chain(stage, time, false);
        let membership: Rc<dyn CollectionMembership> = input.stage.clone();
        let top = MaterialNodeResolvingSceneIndex::new(LightLinkingSceneIndex::new(
            input.chain.clone(),
            Some(membership),
        ));
        Single { input, top }
    }
}

/// A streamed import's index: light linking over a merging index of the
/// payload-free scene input and the chunk being walked, and the
/// material-node resolution last, which fills a network's node outside the
/// chunk from the merged inputs (`moho::chain::merged`'s shape).
pub(super) struct Streamed {
    merging: Rc<MergingSceneIndex>,
    /// The scene input, weakest and permanent.
    scene: Input,
    /// The side input (design D4): a stage masked to the prims the chunks
    /// depend on and no input holds (`moho::streaming::unresolved`),
    /// weaker than the chunk and stronger than the scene input, with what
    /// its stage is masked to.
    side: std::cell::RefCell<Option<(Input, Vec<sdf::Path>)>>,
    /// The top of the chain: what the material readers read, and every
    /// reader once the families move.
    pub(super) top: SceneIndexHandle,
    time: Option<f64>,
}

impl Streamed {
    /// Over the payload-free scene input ([`input_chain`] over the import's
    /// index stage, unloaded prims included).
    pub(super) fn new(scene: Input, time: Option<f64>) -> Self {
        let merging = MergingSceneIndex::new();
        merging.add_input_scene(scene.chain.clone(), sdf::Path::abs_root());
        let input: SceneIndexHandle = merging.clone();
        let top = MaterialNodeResolvingSceneIndex::new(LightLinkingSceneIndex::new(input, None));
        Streamed {
            merging,
            scene,
            side: std::cell::RefCell::new(None),
            top,
            time,
        }
    }

    /// Adds the chunk at `root`, read from its masked `stage`, as the
    /// strongest input.
    pub(super) fn add_chunk(&self, stage: Stage, root: &sdf::Path) -> Input {
        let chunk = input_chain(stage, self.time, false);
        self.merging
            .insert_input_scene(0, chunk.chain.clone(), root.clone());
        chunk
    }

    /// Removes a chunk added by [`Streamed::add_chunk`]; its stage is freed
    /// when the returned input is dropped.
    pub(super) fn remove_chunk(&self, chunk: &Input) {
        self.merging.remove_input_scene(&chunk.chain);
    }

    /// Grows the side input until the merged index holds everything the
    /// chunk just added depends on, or a round resolves nothing new: each
    /// round opens a stage (`open`) masked to what the side stage held and
    /// to every unresolved path not under it, and replaces the side input
    /// with one over that stage, inserted after the chunk. A path no stage
    /// holds (a dangling binding) stays unresolved and, once masked in,
    /// opens nothing more. At most `rounds` opens per chunk. Returns the
    /// side stage and its mask when they changed.
    pub(super) fn resolve_outside(
        &self,
        rounds: usize,
        open: &dyn Fn(Vec<sdf::Path>) -> Option<Stage>,
    ) -> Option<(Stage, Vec<sdf::Path>)> {
        let mut changed = None;
        for _ in 0..rounds {
            let mask: Vec<sdf::Path> = self
                .side
                .borrow()
                .as_ref()
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
            let covered = |p: &sdf::Path| mask.iter().any(|m| p.has_prefix(m));
            let missing: Vec<sdf::Path> = moho::streaming::unresolved(self.merging.as_ref())
                .into_iter()
                .filter(|p| !covered(p))
                .collect();
            if missing.is_empty() {
                break;
            }
            let mut paths = mask.clone();
            paths.extend(missing);
            paths.sort();
            paths.dedup();
            let Some(stage) = open(paths.clone()) else {
                break;
            };
            let input = input_chain(stage.clone(), self.time, false);
            if let Some((old, _)) = self.side.borrow_mut().take() {
                self.merging.remove_input_scene(&old.chain);
            }
            // After the chunk (position 0), before the scene input.
            self.merging
                .insert_input_scene(1, input.chain.clone(), sdf::Path::abs_root());
            *self.side.borrow_mut() = Some((input, paths.clone()));
            changed = Some((stage, paths));
        }
        changed
    }

    /// Removes the side input, after the last chunk.
    pub(super) fn remove_side(&self) {
        if let Some((old, _)) = self.side.borrow_mut().take() {
            self.merging.remove_input_scene(&old.chain);
        }
    }

    /// The scene input, for the stage facts and the materials it holds.
    pub(super) fn scene(&self) -> &Input {
        &self.scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Cornell box read through the bridge chain lists its prims with
    /// their Hydra types.
    #[test]
    fn the_cornell_box_lists_its_prims_with_hydra_types() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/cornellbox.usda");
        let stage = super::super::stage_builder()
            .load(moho::openusd::usd::InitialLoadSet::LoadAll)
            .open(path)
            .expect("the Cornell box opens");
        let single = Single::new(stage, None);
        let mut types = std::collections::BTreeMap::<String, usize>::new();
        let mut stack = vec![sdf::Path::abs_root()];
        while let Some(path) = stack.pop() {
            let ty = single.top.prim(&path).ty;
            if !ty.as_str().is_empty() {
                *types.entry(ty.as_str().to_owned()).or_default() += 1;
            }
            stack.extend(single.top.children(&path));
        }
        assert!(types.get("mesh").is_some_and(|n| *n >= 5), "{types:?}");
        assert_eq!(types.get("camera"), Some(&1), "{types:?}");
        assert!(types.contains_key("material"), "{types:?}");
    }
}
