//! The walk over the scene index (design D5): from the absolute root, over the
//! index's children, with the direct traversal's stack discipline (push in
//! order, pop last-first), because geometry ids, light-link runs, pass flags
//! and provenance depend on visit order, and bitwise image parity on geometry
//! order.
//!
//! The children come from the import's one terminal scene index. Pruning
//! reads the index: an abstract or inactive prim is absent from it, and
//! `purpose` and `visibility` are Hydra's flattened values
//! (`purpose/purpose`, `visibility/visibility`), so a child's own authored
//! purpose overrides an ancestor's `proxy`, as `HdFlattenedPurposeDataSourceProvider`
//! has it, and `invisible` is inherited. Each prim is judged alone.

use moho::data_source::locate;
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::scene_index::SceneIndex;

use super::super::{ImportCtx, Prune, visit_prim};
use super::Input;

/// Walks `input`'s chain into `ctx`, reading each prim's families through the
/// bridge on `stage`, the stage `input` was built over. `top` is the top of
/// the import's chain, over `input` (and, streamed, the other inputs): what
/// the material readers read.
pub(in crate::scene) fn walk(
    input: &Input,
    top: &moho::scene_index::SceneIndexHandle,
    ctx: &mut ImportCtx,
) {
    let index: &dyn SceneIndex = input.chain.as_ref();
    if let Some(pass) = &mut ctx.pass {
        pass.begin_chunk(input);
    }
    ctx.caches.scene = Some(input.chain.clone());
    ctx.caches.top = Some(top.clone());
    ctx.caches.inst = Some(input.chain.clone());
    let mut stack: Vec<(sdf::Path, bool)> = vec![(sdf::Path::abs_root(), false)];
    while let Some((path, parent_hidden)) = stack.pop() {
        let pruned = prune(index, &path);
        let Some(hidden) = visit_prim(index, &path, pruned, parent_hidden, ctx) else {
            continue;
        };
        for child in children(index, &path) {
            stack.push((child, hidden));
        }
    }
    ctx.caches.scene = None;
    ctx.caches.top = None;
    ctx.caches.inst = None;
}

/// The children the walk descends into: the index's, less the prototypes
/// the stage index lists under the absolute root (as UsdImaging does), which
/// the direct traversal never reached from the root — an instance takes its
/// prototype's geometry through the instancing reader.
pub(in crate::scene) fn children(index: &dyn SceneIndex, path: &sdf::Path) -> Vec<sdf::Path> {
    let mut children = index.children(path);
    // A prototype's prims (`instancedBy`) are placed through their instancer:
    // the instancing reader reaches them from the instance or the point
    // instancer.
    children.retain(|c| super::instancers::instanced_by(index, c).is_empty());
    children
}

/// Why the walk leaves the prim at `path` out, from Hydra's flattened values
/// in the index: a `purpose` a render does not draw (`proxy`, `guide`), or
/// `visibility` `invisible`. Missing data reads as its default: an absent
/// purpose is `default`, an absent visibility visible.
pub(in crate::scene) fn prune(index: &dyn SceneIndex, path: &sdf::Path) -> Option<Prune> {
    use moho::tokens::imageable as t;
    if path.is_abs_root() {
        return None;
    }
    let data = index.prim(path).data?;
    let token =
        |schema: &str, name: &str| match locate(&data, &Locator::new([schema, name]))?.value()? {
            moho::openusd::sdf::Value::Token(t) => Some(t.as_str().to_owned()),
            moho::openusd::sdf::Value::String(s) => Some(s),
            _ => None,
        };
    match token(t::PURPOSE, t::PURPOSE).as_deref() {
        Some("proxy") => return Some(Prune::Purpose("proxy")),
        Some("guide") => return Some(Prune::Purpose("guide")),
        _ => {}
    }
    let visible = match locate(&data, &Locator::new([t::VISIBILITY, t::VISIBILITY]))
        .and_then(|d| d.value())
    {
        Some(moho::openusd::sdf::Value::Token(v)) => v.as_str() != t::INVISIBLE,
        Some(moho::openusd::sdf::Value::Bool(v)) => v,
        _ => true,
    };
    (!visible).then_some(Prune::Invisible)
}
