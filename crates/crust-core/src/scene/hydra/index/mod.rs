//! The delegate's view of the host's scene index (design D1, D4): the
//! renderer filters it adds over the host's `moho::chain::standard`, and the
//! readers' accessors over the result (prim data, fields, primvars, material
//! networks, instancers), with what only the host knows ([`host`]).
//!
//! # Layout
//!
//! The host builds `moho::chain::standard` over its stage (the batch host adds
//! the scene globals filter, which carries the time code); the
//! delegate adds Moho's light linking and dependency forwarding over it as
//! renderer filters, as hdPrman and arnold-usd register them ([`Input`]).
//! openusd is named through Moho's re-exports here, so the types that cross
//! the Moho boundary are visible as such (design D2).
//!
//! Every reader reads the scene index alone; what is not scene description
//! comes from the host's [`host::HostFacts`] (design D7).

use moho::chain::{self, RendererFilter};
use moho::scene_index::SceneIndexHandle;

pub(in crate::scene) mod data;
pub(in crate::scene) mod fields;
pub(in crate::scene) mod host;
pub(in crate::scene) mod instancers;
pub(in crate::scene) mod network;
pub(in crate::scene) mod primvars;

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

#[cfg(test)]
mod tests {
    use moho::openusd::sdf;

    /// The Cornell box read through the delegate's chain lists its prims with
    /// their Hydra types.
    #[test]
    fn the_cornell_box_lists_its_prims_with_hydra_types() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/cornellbox.usda");
        let stage = crate::scene::usd_import::stage_builder()
            .load(moho::openusd::usd::InitialLoadSet::LoadAll)
            .open(path)
            .expect("the Cornell box opens");
        let delegate = super::super::delegate::HydraScene::new(
            crate::scene::usd_import::standard_chain(&stage, None),
        );
        delegate.update();
        let top = delegate.index();
        let mut types = std::collections::BTreeMap::<String, usize>::new();
        let mut stack = vec![sdf::Path::abs_root()];
        while let Some(path) = stack.pop() {
            let ty = top.prim(&path).ty;
            if !ty.as_str().is_empty() {
                *types.entry(ty.as_str().to_owned()).or_default() += 1;
            }
            stack.extend(top.children(&path));
        }
        assert!(types.get("mesh").is_some_and(|n| *n >= 5), "{types:?}");
        assert_eq!(types.get("camera"), Some(&1), "{types:?}");
        assert!(types.contains_key("material"), "{types:?}");
    }
}
