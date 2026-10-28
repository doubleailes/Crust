//! The commit (design D3): every managed prim, in `sdf::Path` order, read
//! into the import's state as the walk read it, so the commit is a function
//! of the managed prims and the index alone. Geometry ids, the light list
//! and the volume regions follow path order.
//!
//! What the walk decided for a subtree, the commit decides for each prim
//! from its ancestors, which path order visits first: a prim under one the
//! commit does not descend into (a native instance, a point instancer, a
//! prototype's prim) is not read, and a prim under an invisible one is read
//! as hidden (its cameras still count).

use moho::data_source::locate;
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::scene_index::{SceneIndex, SceneIndexHandle};

use crate::live::LightOrigin;
use crate::warning;
use glam::Mat4 as GMat4;
use tracing::debug;

use super::camera::build_camera;
use super::implicit::{Shape, undisplaced};
use super::instancing::{emit_native_instance, emit_point_instancer};
use super::materials::{resolve_bound, resolve_material};
use super::mesh::{MeshPlace, emit_mesh, place_mesh_source, with_constant_primvars};
use super::settings::CameraChoice;
use super::shapes::{curve_segments, emit_curves, emit_sphere, nurbs_curve_segments};
use super::strands::StrandRequest;
use super::volume::emit_volume;
use super::{ImportCtx, Prune, emit_light, index, volume, xform};

/// Reads the prims at `paths` (the managed prims, in path order) of `index`
/// into `ctx`.
pub(in crate::scene) fn visit_all(
    index: &SceneIndexHandle,
    paths: &[sdf::Path],
    ctx: &mut ImportCtx,
) {
    let chain: &dyn SceneIndex = index.as_ref();
    if let Some(pass) = &mut ctx.pass {
        pass.begin_chunk(&super::index::Input {
            chain: index.clone(),
        });
    }
    ctx.caches.scene = Some(index.clone());
    ctx.caches.top = Some(index.clone());
    ctx.caches.inst = Some(index.clone());
    // The ancestors of the prim being read, each with whether the commit
    // descends into it and whether it is hidden.
    let mut ancestors: Vec<(sdf::Path, bool, bool)> = Vec::new();
    for path in paths {
        while ancestors
            .last()
            .is_some_and(|(a, ..)| !(path.has_prefix(a) && path != a))
        {
            ancestors.pop();
        }
        if ancestors.iter().any(|(_, descends, _)| !descends) {
            continue;
        }
        if !path.is_abs_root() && !super::index::instancers::instanced_by(chain, path).is_empty() {
            // A prototype's prim is placed through its instancer.
            ancestors.push((path.clone(), false, false));
            continue;
        }
        let parent_hidden = ancestors.last().is_some_and(|(.., hidden)| *hidden);
        let pruned = prune(chain, path);
        let (descends, hidden) = match commit_prim(chain, path, pruned, parent_hidden, ctx) {
            None => (false, false),
            Some(hidden) => (true, hidden),
        };
        ancestors.push((path.clone(), descends, hidden));
    }
    ctx.caches.scene = None;
    ctx.caches.top = None;
    ctx.caches.inst = None;
}

/// One prim of the walk over the scene index ([`index::walk`]), under `pruned`,
/// the walk's verdict on it alone (Hydra's flattened values carry the
/// inheritance): emits what it holds into `ctx`, and returns what its children inherit
/// (the world matrix, whether they are under an invisible ancestor, the
/// timing over the shutter), or `None` when the walk does not descend.
pub(in crate::scene) fn commit_prim(
    index: &dyn moho::scene_index::SceneIndex,
    path: &sdf::Path,
    pruned: Option<Prune>,
    parent_hidden: bool,
    ctx: &mut ImportCtx,
) -> Option<bool> {
    let xform = ctx.caches.prim_data(path);
    // A native instance, by its prototype in `__usdPrimInfo`.
    let prototype = {
        use moho::tokens::usd_prim_info as info;
        match ctx
            .caches
            .prim_data(path)
            .locate([info::USD_PRIM_INFO, info::NI_PROTOTYPE_PATH])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::PathVec(mut v)) => v.pop(),
            _ => None,
        }
    };
    // Hydra's flattened transform, and what Hydra does not carry about it.
    let this_world = xform::world(&xform);
    xform::xform_diagnostics(ctx.caches.host, path, ctx.caches.time);
    if let Some(sources) = &mut ctx.arc_sources {
        record_arc_sources(ctx.caches.host, &ctx.caches.prim_data(path), sources);
    }

    // An invisible subtree draws nothing and lights nothing, but is still
    // walked for cameras: a camera's own visibility only hides its gizmo
    // in a viewport, and a rig hidden that way is still rendered through.
    // Pruning it outright would make `--camera` fail on it and move the
    // first-camera fallback. Instances are not entered — crust never
    // takes a camera from a prototype.
    let hidden = parent_hidden || pruned == Some(Prune::Invisible);
    // A purpose a render does not draw (Hydra's flattened purpose, which a
    // child's own authored purpose may override): the prim emits nothing,
    // camera included, and the walk goes on to its children.
    if let Some(reason @ Prune::Purpose(_)) = pruned {
        debug!("Skipping {reason} prim {}", path);
        if prototype.is_some() {
            return None;
        }
        return Some(hidden);
    }
    if hidden {
        if !parent_hidden {
            debug!("Skipping invisible prim {} and its subtree", path);
        }
        if prototype.is_some() {
            return None;
        }
        visit_camera(index, path, ctx);
        return Some(true);
    }

    let timing = ctx.caches.motion.timing(&xform, this_world);
    let span = ctx.caches.motion.span(this_world, &timing);

    // What this prim emits, for the light links: ids are handed out in
    // traversal order, so the range recorded after dispatch is exactly
    // this prim's (see `light_links`).
    let geoms_before = ctx.world.count();
    let vols_before = ctx.volumes.len();
    let lights_before = ctx.lights.count();

    // Native instancing: an `instanceable` prim with a composition arc
    // shares one prototype with every other instance of it. Take the
    // geometry from the prototype and place it — never descend into
    // the instance's own (proxy) subtree, which would rebuild the same
    // triangles once per instance.
    if let Some(proto_path) = prototype {
        ctx.caches.motion.drawn(path, &timing);
        emit_native_instance(&mut ctx.world, path, &proto_path, span, &mut ctx.caches);
        saw(ctx, path, geoms_before..ctx.world.count(), 0..0);
        return None;
    } else if ctx.caches.host.instance_without_prototype(path) {
        warning!(
            InstancingInstanceableWithoutPrototype,
            at = path,
            "Prim {} is instanceable but has no prototype — importing directly",
            path
        );
    }

    // Dispatch by schema. Volume prims are checked first: a prim
    // carrying `primvars:crust:volume:type` imports as a participating-media
    // region only — never as geometry, so its bounds cannot occlude
    // shadow rays, and a volume `Cube` is never a box. The geometry
    // branches then go in the prototype walk's order
    // (`collect_proto_parts`), and a geometry prim no branch reads is
    // counted for `geom.unsupported_type`.
    if ctx.caches.prim_data(path).is_a("PointInstancer") {
        ctx.caches.motion.drawn(path, &timing);
        emit_point_instancer(&mut ctx.world, path, span, &mut ctx.caches);
        saw(ctx, path, geoms_before..ctx.world.count(), 0..0);
        // Prototypes are conventionally authored beneath the
        // instancer; they are drawn through it, never on their own.
        return None;
    } else if volume::volume_type(&ctx.caches.prim_data(path)).is_some() {
        ctx.caches.motion.unmoved(path, &timing);
        let data = ctx.caches.prim_data(path);
        emit_volume(&data, this_world, &mut ctx.volumes, ctx.caches.working);
    } else if ctx.caches.prim_data(path).is_a("Mesh") {
        ctx.caches.motion.drawn(path, &timing);
        let mat = resolve_bound(path, &mut ctx.caches);
        let data = ctx.caches.prim_data(path);
        let baked_moving = emit_mesh(
            &mut ctx.world,
            &data,
            &data,
            span,
            mat,
            &mut ctx.caches.meshes,
            &mut ctx.pending_meshes,
        );
        if baked_moving {
            ctx.caches.motion.ignored.saw(path);
        }
    } else if ctx.caches.prim_data(path).is_a("Sphere") {
        ctx.caches.motion.drawn(path, &timing);
        let mat = resolve_material(path, &mut ctx.caches);
        let data = ctx.caches.prim_data(path);
        if let Some(id) = emit_sphere(&mut ctx.world, &data, span, mat, ctx.caches.working) {
            ctx.spheres.push(id);
        }
    } else if ctx.caches.prim_data(path).is_a("BasisCurves") {
        ctx.caches.motion.unmoved(path, &timing);
        let mat = resolve_material(path, &mut ctx.caches);
        let request = StrandRequest::of(&*mat, ctx.caches.working, &ctx.caches.meshes.primvars);
        let data = ctx.caches.prim_data(path);
        let imported = curve_segments(&data, &data, request.as_ref());
        emit_curves(&mut ctx.world, &data, imported, this_world, mat);
    } else if ctx.caches.prim_data(path).is_a("NurbsCurves") {
        ctx.caches.motion.unmoved(path, &timing);
        let mat = resolve_material(path, &mut ctx.caches);
        let request = StrandRequest::of(&*mat, ctx.caches.working, &ctx.caches.meshes.primvars);
        let data = ctx.caches.prim_data(path);
        let imported = nurbs_curve_segments(&data, &data, request.as_ref());
        emit_curves(&mut ctx.world, &data, imported, this_world, mat);
    } else if let Some(shape) = Shape::read(&ctx.caches.prim_data(path)) {
        ctx.caches.motion.drawn(path, &timing);
        let data = ctx.caches.prim_data(path);
        let bound = undisplaced(&data, resolve_bound(path, &mut ctx.caches));
        let src = ctx
            .caches
            .implicit
            .source(path, &shape, MeshPlace::World(&span.open));
        let data = ctx.caches.prim_data(path);
        let src = with_constant_primvars(src, &data, &*bound.material, ctx.caches.working);
        let baked_moving = place_mesh_source(
            &mut ctx.world,
            &data,
            src,
            span,
            bound,
            &mut ctx.caches.meshes,
            &mut ctx.pending_meshes,
        );
        if baked_moving {
            ctx.caches.motion.ignored.saw(path);
        }
    } else if visit_camera(index, path, ctx) {
        // Recorded (and built, if it is the one) inside the call. A
        // moving camera is out of scope: it renders from the frame.
    } else if emit_walked_light(ctx, path, this_world) {
        ctx.caches.motion.unmoved(path, &timing);
    } else {
        ctx.caches.unread.saw(&ctx.caches.prim_data(path));
    }
    saw(
        ctx,
        path,
        geoms_before..ctx.world.count(),
        vols_before..ctx.volumes.len(),
    );
    // Every light adds at most one entry, so a grown list means `prim`
    // is the light that grew it.
    if ctx.lights.count() > lights_before {
        let Some(scene) = ctx.caches.scene.as_ref() else {
            unreachable!("a light met outside a walk of the scene index")
        };
        ctx.links.light_added(scene.as_ref(), path, lights_before);
        if let Some(pass) = &ctx.pass {
            pass.light_added(path, lights_before, &mut ctx.lights);
        }
        ctx.light_origins.push((
            path.to_string(),
            LightOrigin {
                index: lights_before,
                geoms: geoms_before as u32..ctx.world.count() as u32,
            },
        ));
    }

    // Descend. The per-prim dispatch above picks up any typed schema the
    // children carry.
    Some(false)
}

/// Records that the prim at `path` emitted the geometries `geoms` and the
/// volume regions `vols`: for the light links, which install them after the
/// walk, and the active pass, which flags them now.
fn saw(
    ctx: &mut ImportCtx,
    path: &sdf::Path,
    geoms: std::ops::Range<usize>,
    vols: std::ops::Range<usize>,
) {
    if let Some(pass) = &mut ctx.pass {
        pass.saw(path, geoms.clone(), vols.clone(), &mut ctx.world);
    }
    let Some(scene) = ctx.caches.scene.as_ref() else {
        unreachable!("a prim met outside a walk of the scene index")
    };
    ctx.links.saw(scene.as_ref(), path, geoms, vols);
}

/// Adds the prims `prim` composes from to `sources`: the paths of its
/// `prim_stack` sites other than its own (a site inside one of its variants
/// is its own), read off the stage (design D7), and a `PointInstancer`'s
/// prototype targets.
fn record_arc_sources(
    host: &dyn index::host::HostFacts,
    prim: &index::data::PrimData,
    sources: &mut Vec<String>,
) {
    let own = prim.path().as_str();
    for path in host.prim_stack_paths(prim.path()) {
        let path = without_variants(&path);
        if path != own && !path.is_empty() {
            sources.push(path);
        }
    }
    if prim.is_a("PointInstancer") {
        for target in super::instancing::instancer_targets(prim) {
            sources.push(target.to_string());
        }
    }
}

/// `path` with each variant selection (`{set=name}`) read as a separator, so
/// a site inside a prim's own variant is its own path.
fn without_variants(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut depth = 0usize;
    for c in path.chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 1 && !out.ends_with('/') {
                    out.push('/');
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim_end_matches('/').to_owned()
}

/// [`emit_light`] for the prim at `path` of the walk.
fn emit_walked_light(ctx: &mut ImportCtx, path: &sdf::Path, world_xf: GMat4) -> bool {
    let light = ctx.caches.prim_data(path);
    emit_light(ctx, &light, world_xf)
}

/// Records `prim` if it is a camera — as a candidate for the error listing
/// alternatives, and built when it is the one to render through. `false` when
/// it is not a camera.
fn visit_camera(
    index: &dyn moho::scene_index::SceneIndex,
    path: &sdf::Path,
    ctx: &mut ImportCtx,
) -> bool {
    if index.prim(path).ty.as_str() != moho::tokens::prim_type::CAMERA {
        return false;
    }
    ctx.cameras_seen.push(path.clone());
    let named = ctx.wanted_camera.as_ref().map(CameraChoice::path);
    let is_named = named == Some(path);
    // A named camera is built when met; otherwise only the first camera
    // of a depth-first walk is, kept aside as the fallback when a camera is
    // named.
    let is_first = ctx.first_camera_path.as_ref() == Some(path);
    if ctx.camera.is_none() && (is_named || is_first) {
        match build_camera(index, ctx.caches.host, path, &ctx.settings, ctx.framing) {
            Some(c) if is_named || named.is_none() => {
                debug!("Imported USD camera at {}", path);
                ctx.camera = Some((c, path.clone()));
            }
            Some(c) => ctx.first_camera = Some((c, path.clone())),
            None => warning!(
                CameraUnreadable,
                at = path,
                "Failed to build camera from {}",
                path
            ),
        }
    }
    true
}

/// Counts the native placements of every prototype over the whole index,
/// into `caches.placements` (adaptive mode, design D3): every prim of
/// `paths` the commit reads (not under an instance or a point instancer, nor
/// a prototype's) and not invisible, a purpose a render does not draw
/// placing nothing itself.
pub(in crate::scene) fn count_placements(
    chain: &dyn SceneIndex,
    paths: &[sdf::Path],
    caches: &mut super::ImportCaches<'_>,
) {
    use moho::tokens::usd_prim_info as info;
    let mut ancestors: Vec<(sdf::Path, bool)> = Vec::new();
    for path in paths {
        while ancestors
            .last()
            .is_some_and(|(a, _)| !(path.has_prefix(a) && path != a))
        {
            ancestors.pop();
        }
        if ancestors.iter().any(|(_, descends)| !descends) {
            continue;
        }
        if !path.is_abs_root() && !super::index::instancers::instanced_by(chain, path).is_empty() {
            ancestors.push((path.clone(), false));
            continue;
        }
        let pruned = prune(chain, path);
        if pruned == Some(Prune::Invisible) {
            ancestors.push((path.clone(), false));
            continue;
        }
        if pruned.is_some() {
            // A purpose a render does not draw: not placed, but its children
            // may be.
            ancestors.push((path.clone(), true));
            continue;
        }
        let prim = super::index::data::PrimData::at(chain, path);
        let world = super::xform::world(&prim);
        let proto = match prim
            .locate([info::USD_PRIM_INFO, info::NI_PROTOTYPE_PATH])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::PathVec(mut v)) => v.pop(),
            _ => None,
        };
        if let Some(proto) = proto {
            // Counted per propagated prototype, the key its parts are cached
            // under: instances of one USD prototype bound differently are
            // different prototypes (design D3b).
            let proto = super::index::instancers::native_prototype(chain, path).unwrap_or(proto);
            // A zero-scale placement draws nothing (`attach_proto_parts`
            // skips it), so it does not make its prototype shared.
            if world.determinant().abs() >= 1e-12 {
                *caches.placements.entry(proto.to_string()).or_default() += 1;
            }
            ancestors.push((path.clone(), false));
            continue;
        }
        if prim.is_a("PointInstancer") {
            for (target, n) in super::instancing::instancer_target_counts(&prim, &world) {
                *caches.placements.entry(target).or_default() += n as u32;
            }
            ancestors.push((path.clone(), false));
            continue;
        }
        ancestors.push((path.clone(), true));
    }
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
    children.retain(|c| super::index::instancers::instanced_by(index, c).is_empty());
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

/// How an update brings the scene up to date (design D9), cheapest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::scene) enum Class {
    /// The batch changed nothing the scene was built from.
    Unchanged,
    /// The render settings, products or vars, or the render camera.
    Settings,
    /// A used material's network.
    Materials,
    /// A light's parameters, a light added or removed, a receiver's
    /// categories.
    Lights,
    /// A mesh's transform or shape, an area light's transform.
    Geometry,
    /// Anything else: the scene is committed afresh.
    Full,
}

impl Class {
    pub(in crate::scene) fn as_str(self) -> &'static str {
        match self {
            Class::Unchanged => "unchanged",
            Class::Settings => "settings",
            Class::Materials => "materials",
            Class::Lights => "lights",
            Class::Geometry => "geometry",
            Class::Full => "full",
        }
    }
}

/// The first element of every locator in `locators`: the schema each one
/// is in (`xform`, `material`, `primvars`, …).
fn schemas(locators: &moho::locator::LocatorSet) -> Vec<String> {
    let mut out: Vec<String> = locators
        .iter()
        .map(|l| {
            l.elements()
                .first()
                .map(|t| t.as_str().to_owned())
                .unwrap_or_default()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn only(schemas: &[String], allowed: &[&str]) -> bool {
    schemas.iter().all(|s| allowed.contains(&s.as_str()))
}

/// D9's class table: the class an update needs, decided from the batch
/// alone — the prims added and removed by kind, and the locators dirtied —
/// and what the committed scene was built from (`prov`). The settings
/// class's guards (the dicing projection, the subdivision level, the
/// shutter, …) need the settings read again, so the update checks them when
/// it applies the class, and goes `full` when one moves. With
/// `CRUST_SCENE_DELTAS=0`, every update is `full`.
pub(in crate::scene) fn classify(
    batch: &super::prims::Batch,
    prov: Option<&crate::live::Provenance>,
) -> Class {
    use super::prims::Kind;
    if batch.is_empty() {
        return Class::Unchanged;
    }
    let Some(prov) = prov.filter(|_| crate::config().scene_deltas) else {
        return Class::Full;
    };
    let mut classes = std::collections::BTreeSet::new();
    for (_, kind) in batch.added.iter().chain(&batch.removed) {
        classes.insert(if kind.is_light() {
            Class::Lights
        } else {
            Class::Full
        });
    }
    for (path, (kind, locators)) in &batch.dirtied {
        let schemas = schemas(locators);
        let at = path.as_str();
        let class = if only(&schemas, &["categories", "instanceCategories"]) {
            // A receiver's or an instancer's light-link categories.
            Class::Lights
        } else {
            match kind {
                Kind::RenderSettings | Kind::RenderProduct | Kind::RenderVar => Class::Settings,
                Kind::Camera => {
                    if prov.camera() == Some(at) && only(&schemas, &["camera", "xform"]) {
                        Class::Settings
                    } else {
                        Class::Full
                    }
                }
                Kind::Material => {
                    if prov.material(at).is_some() && only(&schemas, &["material"]) {
                        Class::Materials
                    } else {
                        Class::Full
                    }
                }
                Kind::InfiniteLight => {
                    if only(&schemas, &["light", "material", "xform"]) {
                        Class::Lights
                    } else {
                        Class::Full
                    }
                }
                Kind::AreaLight => {
                    if only(&schemas, &["light", "material"]) {
                        Class::Lights
                    } else if only(&schemas, &["light", "material", "xform"]) {
                        Class::Geometry
                    } else {
                        Class::Full
                    }
                }
                Kind::Mesh => {
                    let moved = locators.iter().all(|l| {
                        let e: Vec<&str> = l.elements().iter().map(|t| t.as_str()).collect();
                        matches!(
                            e.as_slice(),
                            ["xform", ..]
                                | ["primvars", "points", ..]
                                | ["primvars", "normals", ..]
                        )
                    });
                    let reshaped = only(&schemas, &["mesh", "primvars"]);
                    if (moved && prov.movable(at).is_none())
                        || (reshaped && prov.reshapable(at).is_none())
                    {
                        Class::Geometry
                    } else {
                        Class::Full
                    }
                }
                // A transform's, a visibility's, a purpose's or a constant
                // primvar's change reaches the prims below it as their own
                // (flattened) dirtied locators.
                Kind::Other if only(&schemas, &["xform", "visibility", "purpose", "primvars"]) => {
                    Class::Unchanged
                }
                _ => Class::Full,
            }
        };
        classes.insert(class);
    }
    classes.remove(&Class::Unchanged);
    match classes.len() {
        0 => Class::Unchanged,
        1 => classes.into_iter().next().unwrap_or(Class::Full),
        _ => Class::Full,
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use moho::filter::prim_type_notice_batching::PrimTypePriority;
    use moho::openusd::tf::Token;

    use crate::scene::hydra::factory::CrustPriority;
    use crate::{Renderer, Scene, UsdImportOptions};

    /// D1's priorities, reversed.
    struct Reversed;

    impl PrimTypePriority for Reversed {
        fn priority(&self, prim_type: &Token) -> usize {
            CrustPriority.num_priorities() - 1 - CrustPriority.priority(prim_type)
        }
        fn num_priorities(&self) -> usize {
            CrustPriority.num_priorities()
        }
    }

    fn render(scene: Scene) -> Vec<u32> {
        let renderer = Renderer::new(scene.camera, scene.world, scene.lights, scene.settings);
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

    /// The commit is a function of the managed prims and the index alone
    /// (design D3): two notice orders commit bitwise-equal scenes.
    #[test]
    fn two_notice_orders_commit_the_same_scene() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
        for sample in [
            "light_linking.usda",
            "instancing.usda",
            "materialx_showcase.usda",
        ] {
            let path = root.join(sample);
            let options = UsdImportOptions::default();
            let load = |priority: Option<Rc<dyn PrimTypePriority>>| {
                let mut scene = crate::scene::usd_import::load_scene_with(
                    &path,
                    &crate::NoAssets,
                    &options,
                    priority,
                )
                .expect("the sample loads");
                scene.settings = scene
                    .settings
                    .with_samples_per_pixel(2)
                    .with_resolution(32, 18);
                scene
            };
            let ordered = load(None);
            let reversed = load(Some(Rc::new(Reversed)));
            assert_eq!(ordered.world.count(), reversed.world.count(), "{sample}");
            assert_eq!(ordered.lights.count(), reversed.lights.count(), "{sample}");
            assert!(render(ordered) == render(reversed), "{sample}");
        }
    }
}
