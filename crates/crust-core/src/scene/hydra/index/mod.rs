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

use moho::chain::{self, RendererFilter};
use moho::scene_index::SceneIndexHandle;

pub(in crate::scene) mod data;
pub(in crate::scene) mod fields;
pub(in crate::scene) mod host;
pub(in crate::scene) mod instancers;
pub(in crate::scene) mod network;
pub(in crate::scene) mod primvars;
pub(in crate::scene) mod walk;

/// One input of the import: a host's index and the chain over it.
pub(in crate::scene) struct Input {
    /// The terminal scene index: the host's `moho::chain::standard`, with
    /// light linking and dependency forwarding over it, as hdPrman and
    /// arnold-usd register them.
    pub(in crate::scene) chain: SceneIndexHandle,
}

impl Input {
    /// The delegate's chain over the host's `standard` chain.
    pub(in crate::scene) fn over(standard: SceneIndexHandle) -> Self {
        let chain = chain::renderer_filters(
            standard,
            &[
                RendererFilter::light_linking(),
                RendererFilter::dependency_forwarding(),
            ],
        );
        Input { chain }
    }
}

/// A single-stage import's index: one input, read at its terminal index.
pub(in crate::scene) struct Single {
    pub(in crate::scene) input: Input,
    /// The top of the chain: what every reader reads.
    pub(in crate::scene) top: SceneIndexHandle,
}

impl Single {
    pub(in crate::scene) fn new(standard: SceneIndexHandle) -> Self {
        let input = Input::over(standard);
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
        let stage = crate::scene::usd_import::stage_builder()
            .load(moho::openusd::usd::InitialLoadSet::LoadAll)
            .open(path)
            .expect("the Cornell box opens");
        let single = Single::new(crate::scene::usd_import::standard_chain(&stage, None));
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
