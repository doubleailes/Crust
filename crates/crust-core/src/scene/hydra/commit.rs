//! The commit (design D3): every managed prim, in `sdf::Path` order, read
//! into the import's state, so the commit is a function of the managed
//! prims and the index alone. Geometry ids, the light list and the volume
//! regions follow path order.
//!
//! What applies to a subtree, the commit decides for each prim from its
//! ancestors, which path order visits first: a prim under one the
//! commit does not descend into (a native instance, a point instancer, a
//! prototype's prim) is not read, and a prim under an invisible one is read
//! as hidden (its cameras still count).

use moho::data_source::locate;
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::scene_index::{SceneIndex, SceneIndexHandle};

use crate::live::{LightOrigin, LightPath};
use crate::warning;
use glam::Mat4 as GMat4;
use tracing::debug;

use super::camera::build_camera;
use super::implicit::{Shape, undisplaced};
use super::instancing::{emit_native_instance, emit_point_instancer};
use super::light_links::LightLinks;
use super::materials::{resolve_bound, resolve_material};
use super::mesh::{MeshPlace, emit_mesh, place_mesh_source, with_constant_primvars};
use super::settings::CameraChoice;
use super::shapes::{curve_segments, emit_curves, emit_sphere, nurbs_curve_segments};
use super::strands::StrandRequest;
use super::volume::emit_volume;
use super::{ImportCtx, Prune, emit_light, volume, xform};

/// Reads the prims at `paths` (the managed prims, in path order) of `index`
/// into `ctx`.
pub(in crate::scene) fn visit_all(
    index: &SceneIndexHandle,
    paths: &[sdf::Path],
    ctx: &mut ImportCtx,
) {
    let chain: &dyn SceneIndex = index.as_ref();
    if let Some(pass) = &mut ctx.pass {
        pass.begin(&super::index::Input {
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
    // The light layer, after every receiver, while the index is live: a
    // light's inputs, its dome texture and its link categories are read now
    // (design D1 of `interactive-updates`).
    ctx.world.begin_light_layer();
    let lights = std::mem::take(&mut ctx.light_prims);
    ctx.layer = Some(emit_light_layer(ctx, &lights));
    ctx.caches.scene = None;
    ctx.caches.top = None;
    ctx.caches.inst = None;
}

/// One managed prim of the commit, under `pruned`, the verdict on it alone
/// ([`prune`]; Hydra's flattened values carry the inheritance): emits what it
/// holds into `ctx`, and returns what its children inherit (the world matrix,
/// whether they are under an invisible ancestor, the timing over the
/// shutter), or `None` when the commit does not descend.
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
    // Hydra's flattened transform.
    let this_world = xform::world(&xform);

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
    } else if is_light(&ctx.caches.prim_data(path)) {
        // Emitted with the light layer, after every receiver
        // ([`emit_light_layer`]).
        ctx.caches.motion.unmoved(path, &timing);
        ctx.light_prims.push((path.clone(), this_world));
    } else {
        ctx.caches.unread.saw(&ctx.caches.prim_data(path));
    }
    saw(
        ctx,
        path,
        geoms_before..ctx.world.count(),
        vols_before..ctx.volumes.len(),
    );
    debug_assert_eq!(
        ctx.lights.count(),
        lights_before,
        "lights wait for the layer"
    );

    // Descend. The per-prim dispatch above picks up any typed schema the
    // children carry.
    Some(false)
}

/// Records that the prim at `path` emitted the geometries `geoms` and the
/// volume regions `vols`: for the light links, which install them after the
/// commit, and the active pass, which flags them now.
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
        unreachable!("a prim met outside a commit of the scene index")
    };
    ctx.links.saw(scene.as_ref(), path, geoms, vols);
}

/// What [`emit_light_layer`] built besides the geometry and the light
/// list: the layer's link record and every light it met.
pub(in crate::scene) struct Layer {
    pub(in crate::scene) links: LightLinks,
    pub(in crate::scene) paths: Vec<LightPath>,
}

/// The light layer (`interactive-updates` design D1): every light of
/// `lights` (its prim and world transform, in path order) emitted by
/// [`emit_light`] into `ctx`'s world and light list, its emitter geometry
/// recorded for the links and the active pass as the walk records a
/// receiver, its link categories read, its origin kept. What a fresh commit
/// runs after its walk and an in-place update runs alone, over a world that
/// starts at the receivers' count: one implementation for both. `ctx`'s
/// caches must hold the index.
pub(in crate::scene) fn emit_light_layer(
    ctx: &mut ImportCtx,
    lights: &[(sdf::Path, GMat4)],
) -> Layer {
    let receivers = std::mem::take(&mut ctx.links);
    let mut paths = Vec::with_capacity(lights.len());
    for (path, world_xf) in lights {
        let geoms_before = ctx.world.count();
        let lights_before = ctx.lights.count();
        let light = ctx.caches.prim_data(path);
        emit_light(ctx, &light, *world_xf);
        let geoms = geoms_before..ctx.world.count();
        saw(ctx, path, geoms.clone(), 0..0);
        let mut links = (None, None);
        // Every light adds at most one entry, so a grown list means `path`
        // is the light that grew it.
        if ctx.lights.count() > lights_before {
            let Some(scene) = ctx.caches.scene.as_ref() else {
                unreachable!("a light emitted outside a read of the scene index")
            };
            links = ctx.links.light_added(scene.as_ref(), path, lights_before);
            if let Some(pass) = &ctx.pass {
                pass.light_added(path, lights_before, &mut ctx.lights);
            }
            ctx.light_origins.push((
                path.to_string(),
                LightOrigin {
                    index: lights_before,
                    geoms: geoms.start as u32..geoms.end as u32,
                },
            ));
        }
        paths.push(LightPath {
            path: path.to_string(),
            geoms: geoms.start as u32..geoms.end as u32,
            links,
        });
    }
    Layer {
        links: std::mem::replace(&mut ctx.links, receivers),
        paths,
    }
}

/// What a fresh commit's walk does with a prim ([`walked_light`]).
pub(in crate::scene) enum Walked {
    /// It is a light the walk sends to the light layer.
    Light,
    /// It is not: no light, or one the walk does not reach or emit.
    Not,
    /// A decision this test does not make, and why.
    Unsure(String),
}

/// Whether the walk of a fresh commit ([`visit_all`], [`commit_prim`]) sends
/// the prim at `path` of `index` to the light layer: a light, every ancestor
/// descended into (no native instance, point instancer or prototype's prim,
/// nothing invisible), and itself drawn (no purpose a render leaves out).
/// What an in-place update asks of a light added or removed.
pub(in crate::scene) fn walked_light(index: &dyn SceneIndex, path: &sdf::Path) -> Walked {
    use moho::tokens::usd_prim_info as info;
    let prim = super::index::data::PrimData::at(index, path);
    if !is_light(&prim) {
        return Walked::Not;
    }
    let mut chain = vec![path.clone()];
    while let Some(parent) = chain.last().and_then(sdf::Path::parent) {
        chain.push(parent);
    }
    for a in chain.iter().rev() {
        if !a.is_abs_root() && !super::index::instancers::instanced_by(index, a).is_empty() {
            return Walked::Not;
        }
        let data = super::index::data::PrimData::at(index, a);
        let pruned = prune(index, a);
        let instance = data
            .locate([info::USD_PRIM_INFO, info::NI_PROTOTYPE_PATH])
            .is_some();
        if a == path {
            if pruned.is_some() {
                return Walked::Not;
            }
            if instance
                || data.is_a("PointInstancer")
                || volume::volume_type(&data).is_some()
                || ["Mesh", "Sphere", "BasisCurves", "NurbsCurves"]
                    .into_iter()
                    .any(|t| data.is_a(t))
                || Shape::read(&data).is_some()
                || index.prim(a).ty.as_str() == moho::tokens::prim_type::CAMERA
            {
                return Walked::Unsure(format!("{path} is a light of another kind too"));
            }
            return Walked::Light;
        }
        if pruned == Some(Prune::Invisible) || instance || data.is_a("PointInstancer") {
            return Walked::Not;
        }
    }
    Walked::Unsure(format!("{path} has no ancestors"))
}

/// Whether `prim` is a light [`emit_light`] emits: the walk's test, which
/// sends it to the light layer.
pub(in crate::scene) fn is_light(prim: &super::index::data::PrimData) -> bool {
    [
        "SphereLight",
        "RectLight",
        "DiskLight",
        "CylinderLight",
        "DistantLight",
    ]
    .into_iter()
    .any(|t| prim.is_a(t))
        || super::lights::DomeInputs::of(prim).is_some()
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
        match build_camera(index, path, &ctx.settings, ctx.framing) {
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

/// The children a depth-first walk of the index descends into
/// ([`first_camera`](super::first_camera), the listing): the index's, less a
/// prototype's prims, which an instance or a point instancer places through
/// the instancing reader.
pub(in crate::scene) fn children(index: &dyn SceneIndex, path: &sdf::Path) -> Vec<sdf::Path> {
    let mut children = index.children(path);
    // A prototype's prims (`instancedBy`) are placed through their instancer:
    // the instancing reader reaches them from the instance or the point
    // instancer.
    children.retain(|c| super::index::instancers::instanced_by(index, c).is_empty());
    children
}

/// Why the commit leaves the prim at `path` out, from Hydra's flattened values
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
pub enum Class {
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
    pub fn as_str(self) -> &'static str {
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

/// Moho's generic data (`__usdAttributes`, `__usdRelationships`), which the
/// delegate does not read: a change there reaches it through the Hydra
/// schemas that carry the value, or not at all.
fn generic(locator: &moho::locator::Locator) -> bool {
    locator
        .elements()
        .first()
        .is_some_and(|t| matches!(t.as_str(), "__usdAttributes" | "__usdRelationships"))
}

/// The first element of every locator in `locators` the delegate reads: the
/// schema each one is in (`xform`, `material`, `primvars`, …).
fn schemas(locators: &moho::locator::LocatorSet) -> Vec<String> {
    let mut out: Vec<String> = locators
        .iter()
        .filter(|l| !generic(l))
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
) -> (Class, super::update::Change) {
    use super::prims::Kind;
    let mut change = super::update::Change::default();
    if batch.is_empty() {
        return (Class::Unchanged, change);
    }
    let Some(prov) = prov.filter(|_| crate::config().scene_deltas) else {
        return (Class::Full, change);
    };
    let mut classes = std::collections::BTreeSet::new();
    for (path, kind) in batch.added.iter().chain(&batch.removed) {
        classes.insert(if kind.is_light() {
            // The light layer is built again, the light in or out of it
            // as the index now says (design D8 of `interactive-updates`).
            change.layer = true;
            change.layer_paths.push(path.to_string());
            Class::Lights
        } else {
            Class::Full
        });
    }
    for (path, (kind, locators)) in &batch.dirtied {
        let schemas = schemas(locators);
        if schemas.is_empty() {
            continue;
        }
        let at = path.as_str();
        let class = if only(&schemas, &["categories", "instanceCategories"]) {
            // A receiver's or an instancer's light-link categories: the
            // links are judged again over every receiver, the layer built
            // again with them.
            change.layer = true;
            change.categories.push(at.to_owned());
            Class::Lights
        } else {
            match kind {
                Kind::RenderSettings | Kind::RenderProduct | Kind::RenderVar => {
                    change.settings = true;
                    Class::Settings
                }
                Kind::Camera => {
                    if prov.camera() == Some(at) && only(&schemas, &["camera", "xform"]) {
                        change.settings = true;
                        Class::Settings
                    } else {
                        Class::Full
                    }
                }
                Kind::Material => {
                    if prov.material(at).is_some() && only(&schemas, &["material"]) {
                        change.materials.push(at.to_owned());
                        Class::Materials
                    } else {
                        Class::Full
                    }
                }
                // A light's collections: its link category ids may move, so
                // the links are judged again, the layer built again with them.
                Kind::InfiniteLight | Kind::AreaLight
                    if schemas.iter().any(|s| s == "collections")
                        && only(&schemas, &["light", "material", "collections"]) =>
                {
                    change.layer = true;
                    Class::Lights
                }
                Kind::InfiniteLight => {
                    if only(&schemas, &["light", "material", "xform"]) {
                        change.lights.push(at.to_owned());
                        Class::Lights
                    } else {
                        Class::Full
                    }
                }
                Kind::AreaLight => {
                    if only(&schemas, &["light", "material"]) {
                        change.lights.push(at.to_owned());
                        Class::Lights
                    } else if only(&schemas, &["light", "material", "xform"]) {
                        // Its emitter moves: the layer is built again.
                        change.layer = true;
                        Class::Geometry
                    } else {
                        Class::Full
                    }
                }
                Kind::Mesh => {
                    // Only a transform moves a mesh: its points and normals
                    // are its shape, which a placement that keeps its
                    // committed (possibly shared) mesh would not read again.
                    // A shape change, with or without a move, reshapes a
                    // baked mesh placed once (read at the transform it has
                    // now), and commits afresh otherwise.
                    if only(&schemas, &["xform"]) && prov.movable(at).is_none() {
                        change.moved.push(at.to_owned());
                        Class::Geometry
                    } else if only(&schemas, &["mesh", "primvars", "xform"])
                        && prov.reshapable(at).is_none()
                    {
                        change.reshaped.push(at.to_owned());
                        Class::Geometry
                    } else if only(&schemas, &["mesh", "primvars"])
                        && let Some(key) = prov.prototype_of(at)
                    {
                        // A mesh of a prototype: its parts are built again and
                        // placed at every placement (design D5).
                        if !change.prototypes.iter().any(|k| k == key) {
                            change.prototypes.push(key.to_owned());
                        }
                        Class::Geometry
                    } else {
                        Class::Full
                    }
                }
                Kind::Sphere => {
                    if only(&schemas, &["xform"]) && prov.movable(at).is_none() {
                        change.moved.push(at.to_owned());
                        Class::Geometry
                    } else {
                        Class::Full
                    }
                }
                // The host's scene globals: a new current frame (the time
                // the settings and the sampler's frame seed are read at), or
                // another active settings prim, pass or camera.
                Kind::Other if path.is_abs_root() && only(&schemas, &["sceneGlobals"]) => {
                    change.settings = true;
                    Class::Settings
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
    let class = match classes.len() {
        0 => Class::Unchanged,
        1 => classes.into_iter().next().unwrap_or(Class::Full),
        _ => Class::Full,
    };
    (class, change)
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

    use moho::data_source::{Container, ContainerHandle, DataSource};
    use moho::openusd::sdf;
    use moho::scene_index::{Observer, Prim, SceneIndex, SceneIndexHandle};

    /// How many prims [`Hiding`] hid data of: the tests check it bit.
    static HIDDEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A scene index hiding the data named `hidden` of every prim `which`
    /// picks: Hydra giving no data there.
    struct Hiding {
        input: SceneIndexHandle,
        which: fn(&sdf::Path) -> bool,
        hidden: &'static [&'static str],
    }

    struct HidingContainer {
        input: ContainerHandle,
        hidden: &'static [&'static str],
    }

    impl Container for HidingContainer {
        fn names(&self) -> Vec<Token> {
            self.input
                .names()
                .into_iter()
                .filter(|n| !self.hidden.contains(&n.as_str()))
                .collect()
        }
        fn get(&self, name: &Token) -> Option<DataSource> {
            if self.hidden.contains(&name.as_str()) {
                return None;
            }
            self.input.get(name)
        }
    }

    impl SceneIndex for Hiding {
        fn prim(&self, path: &sdf::Path) -> Prim {
            let mut prim = self.input.prim(path);
            if (self.which)(path) && prim.data.is_some() {
                HIDDEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                prim.data = prim.data.map(|input| {
                    Rc::new(HidingContainer {
                        input,
                        hidden: self.hidden,
                    }) as ContainerHandle
                });
            }
            prim
        }
        fn children(&self, path: &sdf::Path) -> Vec<sdf::Path> {
            self.input.children(path)
        }
        fn add_observer(&self, observer: std::rc::Weak<dyn Observer>) {
            self.input.add_observer(observer);
        }
    }

    /// The sample-free stage `text` written to `name`, loaded with `hide`'s
    /// prims hiding `hidden`, rendered at 2 spp.
    fn load_hiding(
        name: &str,
        text: &str,
        which: fn(&sdf::Path) -> bool,
        hidden: &'static [&'static str],
    ) -> (Vec<u32>, Vec<crate::WarningCode>, usize) {
        let dir = std::env::temp_dir().join("crust_missing_data");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write stage");
        let mut scene = crate::scene::usd_import::load_scene_with(
            &path,
            &crate::NoAssets,
            &UsdImportOptions::default(),
            crate::scene::usd_import::Tweaks {
                priority: None,
                wrap: Some(Box::new(move |input| {
                    Rc::new(Hiding {
                        input,
                        which,
                        hidden,
                    }) as SceneIndexHandle
                })),
            },
        )
        .expect("loads");
        scene.settings = scene
            .settings
            .with_samples_per_pixel(2)
            .with_resolution(32, 18);
        let codes = scene.warnings.iter().map(|w| w.code).collect();
        let geometries = scene.world.count();
        (render(scene), codes, geometries)
    }

    const INSTANCES: &str = r#"#usda 1.0
class "Lib"
{
    def Xform "P"
    {
        def Sphere "S" { double radius = 0.5 }
    }
}
def Xform "W"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "A" (instanceable = true references = </Lib/P>)
    {
        double3 xformOp:translate = (-1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "B" (instanceable = true references = </Lib/P>)
    {
        double3 xformOp:translate = (1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def DomeLight "Sky" {}
}
"#;

    /// scene-index-import spec, "A native instancer without flattened data":
    /// its instances are placed by their instance transforms alone and
    /// render visible, with no warning.
    #[test]
    fn a_native_instancer_without_flattened_data_places_its_instances() {
        let with = load_hiding("instancer.usda", INSTANCES, |_| false, &[]);
        let before = HIDDEN.load(std::sync::atomic::Ordering::Relaxed);
        let without = load_hiding(
            "instancer_bare.usda",
            INSTANCES,
            |p| p.as_str().contains("UsdNiInstancer"),
            &["xform", "visibility"],
        );
        assert!(with.1.is_empty(), "{:?}", with.1);
        assert!(without.1.is_empty(), "{:?}", without.1);
        assert!(
            HIDDEN.load(std::sync::atomic::Ordering::Relaxed) > before,
            "the instancer's data was hidden"
        );
        assert_eq!(without.2, 2, "both instances are placed");
        assert!(without.0.iter().any(|b| *b != 0), "something renders");
        assert!(with.0 == without.0, "the instances render as with the data");
    }

    /// scene-index-import spec, "A light with no transform": placed at the
    /// origin with the identity orientation.
    #[test]
    fn a_light_with_no_transform_is_at_the_origin() {
        let stage = |translate: &str| {
            format!(
                r#"#usda 1.0
def Camera "Cam"
{{
    double3 xformOp:translate = (0, 0, 8)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}}
def Mesh "Floor"
{{
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    point3f[] points = [(-5, -1, -5), (5, -1, -5), (5, -1, 5), (-5, -1, 5)]
}}
def SphereLight "Key"
{{
    float inputs:radius = 0.2
    float inputs:intensity = 20
    {translate}
}}
"#
            )
        };
        let moved = stage(
            "double3 xformOp:translate = (3, 2, 0)\n    uniform token[] xformOpOrder = [\"xformOp:translate\"]",
        );
        let without = load_hiding(
            "light_bare.usda",
            &moved,
            |p| p.as_str() == "/Key",
            &["xform"],
        );
        let at_origin = load_hiding("light_origin.usda", &stage(""), |_| false, &[]);
        assert!(without.1.is_empty(), "{:?}", without.1);
        assert!(without.0.iter().any(|b| *b != 0), "the light lights");
        assert!(without.0 == at_origin.0, "the light is at the origin");
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
                    crate::scene::usd_import::Tweaks {
                        priority,
                        wrap: None,
                    },
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
