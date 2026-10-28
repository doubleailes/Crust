//! The import through Moho, the Hydra-2.0-style scene index over openusd
//! (the `moho-import` change): the scene indices the import reads a stage
//! through, and the walk over them ([`walk`]).
//!
//! # Layout (design D4)
//!
//! One stage: one [`Input`] over the `LoadAll` stage, `moho::chain::standard`
//! over it, and Moho's light linking and dependency forwarding as renderer
//! filters, as hdPrman and arnold-usd register them.
//!
//! Crust opens every stage itself, with its own `stage_builder()` (the schema
//! registry, the `mmap` resolver), and hands it to the index. openusd is named
//! through Moho's re-exports here, so the types that cross the Moho boundary
//! are visible as such (design D2).
//!
//! Every reader reads the scene index alone; what is not imaging data is
//! read off the stage in one place, [`stage_facts`] (design D7).

use std::rc::Rc;

use moho::chain::{self, RendererFilter};
use moho::openusd::usd::{Stage, TimeCode};
use moho::scene_index::SceneIndexHandle;
use moho::stage::StageSceneIndex;

pub(super) mod data;
pub(super) mod fields;
pub(super) mod instancers;
pub(super) mod network;
pub(super) mod stage_facts;
pub(super) mod walk;

/// One input of the import: a stage's scene index and the chain over it.
pub(super) struct Input {
    /// The stage index, which owns the stage while the input lives.
    pub(super) stage: Rc<StageSceneIndex>,
    /// The terminal scene index: `moho::chain::standard` over the stage
    /// index, with light linking and dependency forwarding over it.
    pub(super) chain: SceneIndexHandle,
}

/// The input over `stage`, at the import's `time` (`None` reads at the index's
/// own time, time code 0, as a new `UsdImagingStageSceneIndex`).
pub(super) fn input_chain(stage: Stage, time: Option<f64>) -> Input {
    let index = StageSceneIndex::new();
    if let Some(t) = time {
        index.set_time(TimeCode::new(t));
    }
    index.set_stage(stage);
    let chain = chain::renderer_filters(
        chain::standard(index.clone()),
        &[
            RendererFilter::light_linking(),
            RendererFilter::dependency_forwarding(),
        ],
    );
    Input {
        stage: index,
        chain,
    }
}

/// A single-stage import's index: one input, read at its terminal index.
pub(super) struct Single {
    pub(super) input: Input,
    /// The top of the chain: what every reader reads.
    pub(super) top: SceneIndexHandle,
}

impl Single {
    pub(super) fn new(stage: Stage, time: Option<f64>) -> Self {
        let input = input_chain(stage, time);
        let top = input.chain.clone();
        Single { input, top }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moho::openusd::sdf;

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
