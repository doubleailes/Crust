//! Instancing: `UsdGeomPointInstancer` and native `instanceable` prims.
//!
//! Both instancing mechanisms — `UsdGeomPointInstancer` and native
//! `instanceable` prims — reduce to the same thing: build a prototype's
//! geometry *once*, then place it many times by transform. The shared
//! currency is a [`ProtoPart`]: one leaf geometry of the prototype, held as
//! a committed kernel scene in its own local space.
//!
//! `World` maps materials per top-level geometry, so a hit has to say which
//! part it landed on. A part therefore carries one *slot* per geometry it
//! can report — its material and texture tables — and a part of several
//! slots is a *group*: one committed scene whose hits are labelled `0..n`
//! (`crust_rt::InstanceHitId`), placed as one instance and given `n`
//! consecutive `geom_id`s. That keeps a many-part prototype one box in the
//! BVH above it. It used to be one instance per part, which is harmless for
//! a handful of parts and ruinous for a scatter of trees of 16 181 branch
//! meshes each: every part then spans the whole scatter, and the Moana
//! island's isDunesB put 64 724 identical boxes over its dune field
//! (`docs/moana_profile.md`).

use crate::warning;
use moho::openusd;
use std::sync::Arc;
use std::time::Instant;

use crust_rt::{
    Geometry, InstanceHitId, RayMask, Scene as RtScene, SceneBuilder as RtSceneBuilder,
};
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::sdf;
use std::collections::{BTreeMap, BTreeSet};
use tracing::debug;

use crate::material::Material;
use crate::primvars::PrimvarTable;
use crate::rt_world::{FaceMap, UvMap, WorldBuilder};

use super::ImportCaches;
use super::attrs::{decode_i32_array, decode_vec3f_array};
use super::implicit::{Shape, undisplaced};
use super::index::data::PrimData;
use super::materials::{resolve_bound_data, resolve_material_data};
use super::mesh::{
    MeshNeeds, MeshPlace, constant_table, mesh_source, placement_scale, with_constant_primvars,
};
use super::shapes::{Curves, curve_segments, curves_scene, nurbs_curve_segments, sphere_radius};
use super::strands::StrandRequest;
use super::xform::Span;
use crate::strands::StrandTable;

/// How deep prototypes may nest before the importer gives up. USD forbids
/// an instancing cycle, but a malformed stage can still describe one, and
/// each level multiplies traversal cost — so this is a backstop, set far
/// above any plausible authoring depth.
const MAX_INSTANCE_NESTING: usize = 8;

/// How a prototype version is placed, which decides how adaptive subdivision
/// refines its meshes (see "Only unshared geometry is adaptive" in the design
/// record). In uniform subdivision every version is [`ProtoPlace::Shared`].
/// A curves prim in a prototype, by which schema it has.
enum Curve {
    Basis,
    Nurbs,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ProtoPlace {
    /// Placed more than once: refined to the uniform level, built once and
    /// shared by every placement.
    Shared,
    /// The one placement of its prototype, whose root frame this carries to
    /// world: refined by its size on screen, like a direct mesh, and built for
    /// that placement alone.
    Unshared(GMat4),
}

/// A prototype as the instancing reader finds it: its root in the input's
/// instancing chain — Moho's propagated prototype
/// (`MohoNiPropagatedPrototypes/<bindings>/<identity>/prototype`) or a point
/// instancer's `ForInstancer` copy — which keys its cached parts, and the USD
/// prim it comes from (`/__Prototype_N`, or the instancer's target), which
/// names it in warnings and at which its prims are read until Moho's M11
/// re-roots the path values inside propagated prototypes.
#[derive(Clone, Debug)]
pub(super) struct ProtoRoot {
    pub(super) index: sdf::Path,
    pub(super) usd: sdf::Path,
}

impl ProtoRoot {
    /// The USD path of the prim at `path` under the index root.
    fn usd_of(&self, path: &sdf::Path) -> sdf::Path {
        if path == &self.index {
            return self.usd.clone();
        }
        path.as_str()
            .strip_prefix(self.index.as_str())
            .and_then(|rest| sdf::Path::new(format!("{}{rest}", self.usd).as_str()).ok())
            .unwrap_or_else(|| path.clone())
    }

    /// The prototype caches' key: the root in the instancing chain, stable
    /// across a streamed import's stages (a propagated prototype, a copy).
    /// No epoch: openusd's per-stage `/__Prototype_N` is never a root the
    /// chain holds, so it keys nothing (a native instance always resolves to
    /// its propagated prototype; checked on every fixture and sample when the
    /// epoch went, `moho-import` 13.1).
    fn key(&self) -> String {
        self.index.to_string()
    }

    /// A root named by its USD path alone, where the instancing chain gives
    /// no other (read as before).
    fn usd(path: &sdf::Path) -> Self {
        ProtoRoot {
            index: path.clone(),
            usd: path.clone(),
        }
    }
}

/// The children the prototype walk descends into: the prim's own, less those
/// another instancer than `instancer` places (`instancedBy`): a nested
/// instancer's prototype, reached through its instance. A nested instance
/// prim carries no `instancedBy` of its own.
fn proto_children(
    caches: &ImportCaches<'_>,
    path: &sdf::Path,
    instancer: &[sdf::Path],
) -> Vec<sdf::Path> {
    let Some(inst) = &caches.inst else {
        return Vec::new();
    };
    inst.children(path)
        .into_iter()
        .filter(|c| {
            let by = super::index::instancers::instanced_by(inst.as_ref(), c);
            by.is_empty() || instancer.is_empty() || by.iter().any(|i| instancer.contains(i))
        })
        .collect()
}

/// What a hit on one geometry of a [`ProtoPart`] resolves to: the material
/// and texture tables `World` keys by `geom_id`.
#[derive(Clone)]
pub(super) struct PartSlot {
    material: Arc<dyn Material>,
    /// Triangle-to-source-face table, when this slot's material samples a
    /// per-face texture.
    ///
    /// A slot is always exactly one leaf geometry — the walk splits per bound
    /// mesh — so one table serves every placement of it, and the `prim_id` a
    /// hit reports indexes that table unambiguously however many levels of
    /// instancing it passed through (the kernel forwards the innermost
    /// `prim_id` unchanged).
    faces: Option<Arc<FaceMap>>,
    /// The slot's texture chart, when its material reads one. Carried on
    /// the same terms as `faces`. A part placed through a group forwards its
    /// id, which `World` cannot trace back to a scene and transform, so
    /// such a hit shades without a tangent frame (normal maps fall back to
    /// the geometric normal there); a direct instance of a mesh gets its
    /// tangent at the hit.
    uvs: Option<Arc<UvMap>>,
    /// The slot's kept primvars, for the AOVs, carried on the same terms as
    /// `faces`. Read through the kernel's own barycentrics, so a grouped
    /// part reads them as a leaf does.
    primvars: Option<Arc<PrimvarTable>>,
    /// A curves slot's strand table, when its material reads the strand, a
    /// primvar or the chart. Carried on the same terms as `faces`: the
    /// `prim_id` a hit reports is the prototype's own span.
    strands: Option<Arc<StrandTable>>,
    /// Whether the slot is curves, table or not ([`WorldBuilder::mark_curves`]).
    curves: bool,
    /// Uniform scale from the slot's own geometry frame, which its tables'
    /// densities are in, to the part's frame. 1 for a leaf part; inside a
    /// group, the member's placement. Nested *instancer* placements are not
    /// folded in (they differ per placement against one table), so such
    /// geometry filters against the outer scale — see the texture filtering
    /// gaps in openspec/specs/textures/design.md.
    scale: f32,
}

/// A piece of a prototype, in the prototype root's frame: a committed
/// kernel scene in its own local space, the transform placing it relative
/// to the prototype root, its visibility mask, and what its hits resolve to.
///
/// With one slot it is a leaf — one bound geometry — and every hit in
/// `scene` is that slot. With several it is a group: `scene` labels its hits
/// with the slot index, and placing it takes as many `geom_id`s.
#[derive(Clone)]
pub(super) struct ProtoPart {
    pub(super) scene: Arc<RtScene>,
    /// Prototype-root-relative placement. An instance's world transform is
    /// composed onto the left of this.
    pub(super) local: GMat4,
    pub(super) mask: RayMask,
    pub(super) slots: Arc<[PartSlot]>,
}

impl ProtoPart {
    /// One bound geometry with its material and tables.
    fn leaf(
        scene: Arc<RtScene>,
        local: GMat4,
        mask: RayMask,
        material: Arc<dyn Material>,
        faces: Option<Arc<FaceMap>>,
        uvs: Option<Arc<UvMap>>,
    ) -> Self {
        ProtoPart {
            scene,
            local,
            mask,
            slots: Arc::new([PartSlot {
                material,
                faces,
                uvs,
                primvars: None,
                strands: None,
                curves: false,
                scale: 1.0,
            }]),
        }
    }

    /// How an instance of this part labels its hits when its first slot is
    /// numbered `first`: a group forwards the inner index on top of it, a
    /// leaf reports `first` whatever its scene says.
    fn label(&self, first: u32) -> InstanceHitId {
        if self.slots.len() > 1 {
            InstanceHitId::Offset(first)
        } else {
            InstanceHitId::As(first)
        }
    }
}

/// Walks a prototype subtree and builds its [`ProtoPart`]s, in the frame an
/// instance places: a native prototype root's own (it contributes no
/// transform), or a `PointInstancer` prototype root's parent's (it
/// contributes its own, see [`part_local`]).
///
/// Abstract (`class`) prims are *not* skipped here, unlike in the main
/// traversal: naming a class as a prototype is exactly how one authors
/// "geometry that exists only to be instanced".
pub(super) fn collect_proto_parts(
    proto: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
    place: ProtoPlace,
) -> Vec<ProtoPart> {
    let mut parts = Vec::new();
    if depth > MAX_INSTANCE_NESTING {
        warning!(
            InstancingNestingTooDeep,
            at = proto.usd,
            "Prototype {} exceeds {MAX_INSTANCE_NESTING} levels of instance nesting — not expanded",
            proto.usd
        );
        return parts;
    }
    let Some(inst) = caches.inst.clone() else {
        return parts;
    };
    // Walked in the instancing chain, each prim read at its re-rooted path
    // and named by its USD path (`ProtoRoot`). A root that is itself an
    // instance (a point instancer's prototype authored instanceable) is read
    // through its own propagated prototype, named under the root.
    let redirected;
    let proto = match super::index::instancers::native_prototype(inst.as_ref(), &proto.index) {
        Some(index) => {
            redirected = ProtoRoot {
                index,
                usd: proto.usd.clone(),
            };
            &redirected
        }
        None => proto,
    };
    // The instancer the prototype's prims are placed by: what keeps a nested
    // instancer's prototype out of this walk.
    let instancer = super::index::instancers::instanced_by(inst.as_ref(), &proto.index);
    // A native prototype's root stands for the instance prim, whose
    // transform the placement already carries.
    let native_root = proto.usd.as_str().starts_with("/__Prototype");
    let mut stack: Vec<sdf::Path> = vec![proto.index.clone()];
    // Mesh parts found by the walk, as (index into `parts`, mesh slot). Their
    // kernel scenes are built together once the walk is done — in parallel,
    // see `MeshArena::commit_slots` — and each part holds a placeholder until
    // then.
    let mut pending_meshes: Vec<(usize, u32)> = Vec::new();

    while let Some(at) = stack.pop() {
        let name = proto.usd_of(&at);
        // Its data: in the instancing chain at its re-rooted path, named by
        // its USD path (Moho's M11 re-roots the path values it holds).
        let pdata = PrimData::rebased(&inst, &at, &proto.index, &proto.usd);
        // The same pruning as the top-level walk, abstractness aside: Hydra's
        // flattened purpose and visibility (an inactive prim the index does
        // not hold). An invisible prim's subtree is invisible too; a purpose
        // a render does not draw may be overridden below.
        let pruned = super::index::walk::prune(inst.as_ref(), &at);
        if let Some(reason) = pruned {
            debug!("Skipping {reason} prim {name} (prototype {})", proto.usd);
        }
        if pruned == Some(super::Prune::Invisible) {
            continue;
        }
        let is_root = at == proto.index;
        // A point instancer prototype's root contributes its own transform,
        // applied before the per-instance one, as
        // `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` composes it
        // by default (`IncludeProtoXform`): composed here rather than folded
        // into each placement, so a descendant whose stack resets drops it
        // like any other ancestor's.
        // Hydra's flattened transform, relative to the prototype root (a
        // native prototype's root resets the stack with the identity, a
        // point instancer's copy root with its own transform).
        let this_local = if is_root && native_root {
            GMat4::IDENTITY
        } else {
            super::xform::world(&pdata)
        };
        if pruned.is_some() {
            for child in proto_children(caches, &at, &instancer) {
                stack.push(child);
            }
            continue;
        }
        if !is_root || !native_root {
            caches.motion.in_prototype(&pdata, &name);
        }

        // A native instance nested in this prototype: splice its prototype's
        // parts in, placed by this prim, rather than walking its proxy
        // subtree — the inner prototype is built once (cached like any
        // other) and its kernel scenes are shared by every outer placement.
        // Always the shared version: `count_placements` does not descend
        // into instances, so nothing says this placement is the only one.
        // Each part keeps its own mask, as at a top-level instance.
        if !is_root {
            use moho::tokens::usd_prim_info as info;
            let inner_usd = match pdata
                .locate([info::USD_PRIM_INFO, info::NI_PROTOTYPE_PATH])
                .and_then(|d| d.value())
            {
                Some(sdf::Value::PathVec(mut v)) => v.pop(),
                _ => None,
            };
            if let (Some(usd), Some(index)) = (
                inner_usd,
                super::index::instancers::native_prototype(inst.as_ref(), &at),
            ) {
                let inner = ProtoRoot { index, usd };
                let inner_parts = prototype_parts(&inner, caches, depth + 1);
                parts.extend(inner_parts.iter().map(|part| ProtoPart {
                    local: this_local * part.local,
                    ..part.clone()
                }));
                continue;
            }
            if caches.host.instance_without_prototype(&name) {
                warning!(
                    InstancingInstanceableWithoutPrototype,
                    at = name,
                    "Prim {} is instanceable but has no prototype — importing directly",
                    name
                );
            }
        }

        let mask = super::index::data::ray_mask(&pdata);

        // Volumes first, as at the top level: a `primvars:crust:volume:*` prim is a
        // volume whatever its schema, so a volume `Cube` must never be read
        // as a box. Volumes live outside the surface BVH entirely (their
        // bounds must not occlude shadow rays), so they cannot ride an
        // instance transform. Say so rather than dropping silently.
        if super::volume::volume_type(&pdata).is_some() {
            warning!(
                VolumeInPrototype,
                at = &name,
                "Volume at {} is inside a prototype — volumes cannot be instanced, skipped",
                &name
            );
        } else if pdata.is_a("Mesh") {
            let bound = resolve_bound_data(&pdata, caches);
            let material = bound.material;
            let data = pdata.clone();
            let displacement = super::mesh::prim_displacement(&data, bound.displacement);
            let displacement = displacement.as_deref();
            // Unshared: the part's own world transform, for adaptive subdivision.
            let part_world = match place {
                ProtoPlace::Unshared(world) => Some(world * this_local),
                ProtoPlace::Shared => None,
            };
            // A prototype part is placed by an instance by definition, so it
            // always needs a real kernel scene — committing here is also what
            // marks the slot as ineligible for baking, so a mesh used both
            // directly and as a prototype is not stored twice.
            if let Some(src) = mesh_source(
                &data,
                &data,
                MeshNeeds::of(
                    &*material,
                    displacement,
                    &caches.meshes.primvars,
                    caches.working,
                ),
                &mut caches.meshes.subdiv,
                part_world
                    .as_ref()
                    .map_or(MeshPlace::Shared, MeshPlace::World),
            ) && let Some(slot) = caches.meshes.intern(&data, src, &material, displacement)
            {
                let faces = caches.meshes.slots[slot as usize].faces.clone();
                let uvs = caches.meshes.slots[slot as usize].uvs.clone();
                let primvars = caches.meshes.slots[slot as usize].primvars.clone();
                pending_meshes.push((parts.len(), slot));
                let mut part =
                    ProtoPart::leaf(placeholder_scene(), this_local, mask, material, faces, uvs);
                Arc::make_mut(&mut part.slots)[0].primvars = primvars;
                parts.push(part);
            }
        } else if pdata.is_a("Sphere") {
            let material = resolve_material_data(&pdata, caches);
            let radius = sphere_radius(&pdata);
            let mut b = RtSceneBuilder::new();
            // Local-space sphere at the origin: unlike the top-level
            // sphere path, which bakes the centre into world space, this
            // lets the instance transform scale it (a non-uniform scale
            // correctly yields an ellipsoid, since rays enter local space).
            b.attach(Geometry::Sphere {
                center: Vec3A::ZERO,
                radius,
            });
            let data = pdata.clone();
            let primvars = constant_table(&data, &*material, caches.working).map(Arc::new);
            let mut part = ProtoPart::leaf(
                Arc::new(b.commit_with(crate::commit_options())),
                this_local,
                mask,
                material,
                None,
                None,
            );
            Arc::make_mut(&mut part.slots)[0].primvars = primvars;
            parts.push(part);
        } else if let Some(read) = {
            let data = pdata.clone();
            if data.is_a("BasisCurves") {
                Some(Curve::Basis)
            } else if data.is_a("NurbsCurves") {
                Some(Curve::Nurbs)
            } else {
                None
            }
        } {
            let material = resolve_material_data(&pdata, caches);
            let request = StrandRequest::of(&*material, caches.working, &caches.meshes.primvars);
            let data = pdata.clone();
            let imported = match &read {
                Curve::Basis => curve_segments(&data, &data, request.as_ref()),
                Curve::Nurbs => nurbs_curve_segments(&data, &data, request.as_ref()),
            };
            if let Some(Curves { batches, strands }) = imported {
                let mut part = ProtoPart::leaf(
                    Arc::new(curves_scene(batches)),
                    this_local,
                    mask,
                    material,
                    None,
                    None,
                );
                let slot = &mut Arc::make_mut(&mut part.slots)[0];
                slot.curves = true;
                slot.strands = strands.map(Arc::new);
                parts.push(part);
            }
        } else if let Some(shape) = Shape::read(&pdata) {
            // An implicit shape is a mesh once tessellated, so it is a part
            // like any mesh: interned, built at the end of the walk.
            let data = pdata.clone();
            let material = undisplaced(&data, resolve_bound_data(&pdata, caches)).material;
            let part_world = match place {
                ProtoPlace::Unshared(world) => Some(world * this_local),
                ProtoPlace::Shared => None,
            };
            let src = caches.implicit.source(
                &name,
                &shape,
                part_world
                    .as_ref()
                    .map_or(MeshPlace::Shared, MeshPlace::World),
            );
            let data = pdata.clone();
            let src = with_constant_primvars(src, &data, &*material, caches.working);
            if let Some(slot) = caches.meshes.intern(&data, src, &material, None) {
                let faces = caches.meshes.slots[slot as usize].faces.clone();
                let uvs = caches.meshes.slots[slot as usize].uvs.clone();
                let primvars = caches.meshes.slots[slot as usize].primvars.clone();
                pending_meshes.push((parts.len(), slot));
                let mut part =
                    ProtoPart::leaf(placeholder_scene(), this_local, mask, material, faces, uvs);
                Arc::make_mut(&mut part.slots)[0].primvars = primvars;
                parts.push(part);
            }
        } else if pdata.is_a("PointInstancer") {
            // A PointInstancer inside a prototype: expand it into nested
            // sub-scenes rather than flattening. Flattening would multiply
            // the *outer* instance count by this instancer's, which is
            // exactly the blow-up instancing exists to avoid — a prototype
            // holding 500 leaves, itself placed 500 times, must stay 500
            // outer instances, not 250 000.
            parts.extend(nested_instancer_parts(
                &pdata,
                &at,
                this_local,
                mask,
                caches,
                depth + 1,
                place,
            ));
            // Its prototypes are reached through it, never drawn directly.
            continue;
        } else {
            caches.unread.saw(&pdata);
        }

        let children = proto_children(caches, &at, &instancer);
        for child in children {
            stack.push(child);
        }
    }
    // Committing is also what marks each slot as ineligible for baking, so a
    // mesh used both directly and as a prototype is not stored twice.
    let slots: Vec<u32> = pending_meshes.iter().map(|&(_, slot)| slot).collect();
    caches.meshes.commit_slots(&slots);
    for (index, slot) in pending_meshes {
        parts[index].scene = caches.meshes.committed_scene(slot);
    }
    parts
}

/// What a prototype's mesh part holds until its kernel scene is built at the
/// end of the walk (see `collect_proto_parts`). Never attached to anything.
fn placeholder_scene() -> Arc<RtScene> {
    static EMPTY: std::sync::OnceLock<Arc<RtScene>> = std::sync::OnceLock::new();
    Arc::clone(
        EMPTY.get_or_init(|| Arc::new(RtSceneBuilder::new().commit_with(crate::commit_options()))),
    )
}

/// Expands a `PointInstancer` found *inside* a prototype into one part.
///
/// Each of its prototypes is grouped into a single scene ([`group_parts`]),
/// and the part is a scene of those groups, placed once per nested instance —
/// so the BVH over it sees one box per placement, which is what makes a
/// scatter cull. Hits come out labelled by slot, the prototypes' slots laid
/// end to end.
///
/// It used to be the other way round: one part per (prototype, part), each
/// a scene of that one piece placed everywhere, because `World` could only
/// tell parts apart by top-level instance. Each such part spans the whole
/// scatter, so a scatter of many-part prototypes became that many identical
/// boxes — 64 724 of them over the Moana island's dunes, 99% of a render's
/// instance descents (`docs/moana_profile.md`).
#[allow(clippy::too_many_arguments)]
fn nested_instancer_parts(
    data: &PrimData,
    at: &sdf::Path,
    local: GMat4,
    mask: RayMask,
    caches: &mut ImportCaches<'_>,
    depth: usize,
    place: ProtoPlace,
) -> Vec<ProtoPart> {
    let Some(mut layout) = read_instancer(data, true) else {
        return Vec::new();
    };
    // Targets the index re-rooted (inside a propagated prototype) are named
    // by the USD prims they come from.
    for target in &mut layout.targets {
        *target = data.name_of(target);
    }
    // Which version of its prototype each placement draws: the shared one
    // (`None`), or — in adaptive mode, inside an unshared prototype, for a
    // target this instancer places once — one built for that placement.
    let counts = target_counts(&layout, &local);
    let adaptive = caches.meshes.subdiv.adaptive.is_some();
    let placement_versions: Vec<(usize, Option<usize>)> = layout
        .placements
        .iter()
        .enumerate()
        .map(|(i, &(k, xf))| {
            let drawn = (local * xf).determinant().abs() >= 1e-12;
            let unshared =
                adaptive && counts[k] == 1 && drawn && matches!(place, ProtoPlace::Unshared(_));
            (k, unshared.then_some(i))
        })
        .collect();
    // Every prototype's shared version in uniform mode, as before (so an
    // unplaced prototype is still built and still reported), and only the
    // placed versions in adaptive mode. In (prototype, version) order, so the
    // build order and the slot layout below are independent of placement
    // order.
    let versions: BTreeSet<(usize, Option<usize>)> = if adaptive {
        placement_versions.iter().copied().collect()
    } else {
        (0..layout.targets.len()).map(|k| (k, None)).collect()
    };
    let roots = proto_roots(caches, at, &layout.targets);
    let groups: BTreeMap<(usize, Option<usize>), Option<ProtoPart>> = versions
        .into_iter()
        .map(|(k, placed)| {
            let target = &roots[k];
            let group = match (placed, place) {
                (Some(i), ProtoPlace::Unshared(world)) => {
                    let inner = world * local * layout.placements[i].1;
                    unshared_group(target, caches, depth, inner)
                }
                _ => prototype_group(target, caches, depth),
            };
            ((k, placed), group)
        })
        .collect();

    // The placements that draw something: a prototype with no geometry, or a
    // zero-scale placement (the "hide this instance" idiom), draws nothing.
    let drawn: Vec<((usize, Option<usize>), GMat4)> = layout
        .placements
        .iter()
        .zip(&placement_versions)
        .filter_map(|(&(_, xf), &version)| {
            let placement = xf * groups[&version].as_ref()?.local;
            (placement.determinant().abs() >= 1e-12).then_some((version, placement))
        })
        .collect();

    // Slots only for the versions something actually draws — every slot is
    // reserved again at each placement of this part, so a prototype placed
    // only by hidden entries would cost ids no hit can reach — in (prototype,
    // version) order so the layout is independent of placement order.
    let mut first: BTreeMap<(usize, Option<usize>), u32> = BTreeMap::new();
    for &(version, _) in &drawn {
        first.insert(version, 0);
    }
    let mut slots: Vec<PartSlot> = Vec::new();
    for (version, f) in first.iter_mut() {
        if let Some(g) = &groups[version] {
            *f = slots.len() as u32;
            slots.extend(g.slots.iter().cloned());
        }
    }

    let mut sub = RtSceneBuilder::new();
    sub.reserve(drawn.len());
    for &(version, placement) in &drawn {
        let (Some(g), Some(&f)) = (&groups[&version], first.get(&version)) else {
            unreachable!("a drawn placement has a group and a slot range");
        };
        sub.attach_labelled(
            Geometry::Instance {
                scene: g.scene.clone(),
                transform: Affine3A::from_mat4(placement),
                transform_end: None,
            },
            g.mask,
            g.label(f),
        );
    }

    debug!(
        "Expanded nested PointInstancer at {} ({} instances of {} prototype(s) -> {} slot(s))",
        data.path(),
        layout.placements.len(),
        layout.targets.len(),
        slots.len()
    );
    if sub.count() == 0 {
        return Vec::new();
    }
    vec![ProtoPart {
        scene: Arc::new(sub.commit_with(crate::commit_options())),
        local,
        mask,
        slots: slots.into(),
    }]
}

/// Puts a prototype's parts into one scene, so it can be placed as one
/// instance: each part becomes a member instance at its prototype-relative
/// transform, labelled with the index of its first slot, and the group's
/// slots are the members' laid end to end. `None` when nothing is left.
///
/// A single part is returned as it is — grouping it would only add a level
/// of instancing.
fn group_parts(parts: &[ProtoPart]) -> Option<ProtoPart> {
    if let [only] = parts {
        return Some(only.clone());
    }
    let mut b = RtSceneBuilder::new();
    b.reserve(parts.len());
    let mut slots: Vec<PartSlot> = Vec::new();
    let mut mask = RayMask::NONE;
    for part in parts {
        if part.local.determinant().abs() < 1e-12 {
            continue;
        }
        let local = Affine3A::from_mat4(part.local);
        b.attach_labelled(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: local,
                transform_end: None,
            },
            part.mask,
            part.label(slots.len() as u32),
        );
        let scale = placement_scale(&local);
        slots.extend(part.slots.iter().map(|slot| PartSlot {
            scale: slot.scale * scale,
            ..slot.clone()
        }));
        // A ray enters the group if any member could take it; each member's
        // own mask then gates it inside.
        mask |= part.mask;
    }
    if slots.is_empty() {
        return None;
    }
    Some(ProtoPart {
        scene: Arc::new(b.commit_with(crate::commit_options())),
        local: GMat4::IDENTITY,
        mask,
        slots: slots.into(),
    })
}

/// A prototype as one part ([`group_parts`]), from the cache or freshly
/// built.
fn prototype_group(
    proto_path: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Option<ProtoPart> {
    let key = proto_path.key();
    if let Some(group) = caches.groups.get(&key) {
        return group.clone();
    }
    let parts = prototype_parts(proto_path, caches, depth);
    let group = group_parts(&parts);
    caches.groups.insert(key, group.clone());
    group
}

/// A prototype as one part, built for its one placement (`world` carries its
/// root frame to world) and not cached: no other placement can use it.
fn unshared_group(
    proto_path: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
    world: GMat4,
) -> Option<ProtoPart> {
    group_parts(&unshared_parts(proto_path, caches, depth, world))
}

/// The parts a top-level placement of `proto_path` attaches: the prototype
/// grouped into one instance when it has at least
/// [`TOP_LEVEL_GROUP_MIN_PARTS`] parts, else its parts one instance each.
fn placed_parts(
    proto_path: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    place: ProtoPlace,
) -> Arc<Vec<ProtoPart>> {
    let parts = match place {
        ProtoPlace::Shared => prototype_parts(proto_path, caches, 0),
        ProtoPlace::Unshared(world) => Arc::new(unshared_parts(proto_path, caches, 0, world)),
    };
    if parts.len() < TOP_LEVEL_GROUP_MIN_PARTS {
        return parts;
    }
    Arc::new(group_parts(&parts).into_iter().collect())
}

/// Parts from which a top-level placement is grouped into one instance.
///
/// Grouping costs every ray that enters the placement one more transform,
/// which a prototype of a few parts does not repay: its parts' boxes at the
/// top level are few and already local to the placement. A prototype of
/// thousands of parts — a Moana bay cedar is 16 181 — puts that many boxes
/// into the root BVH per placement. The threshold is a round number between
/// the two, not a measured optimum; nested instancers group always, since
/// there each ungrouped part spans the whole scatter.
const TOP_LEVEL_GROUP_MIN_PARTS: usize = 64;

/// Imports one natively-instanced prim (`instanceable = true` plus a
/// composition arc) by placing its shared prototype's parts.
///
/// This is the mechanism Moana-scale scenes rely on, and the reason it
/// matters is memory: without it the importer walks each instance's proxy
/// subtree and re-reads its geometry, so cost scales with the *instance*
/// count. Here every instance of a prototype shares one set of committed
/// kernel scenes, and costs only its transforms.
pub(super) fn emit_native_instance(
    world: &mut WorldBuilder,
    path: &sdf::Path,
    proto_path: &sdf::Path,
    span: Span,
    caches: &mut ImportCaches<'_>,
) {
    // A kept primvar on the instance, or above it, does not reach the
    // prototype's prims, which read only their own.
    caches
        .meshes
        .primvars
        .report_placement(&caches.prim_data(path), "the instance prim");
    let world_xf = span.open;
    let root = match caches
        .inst
        .as_ref()
        .and_then(|inst| super::index::instancers::native_prototype(inst.as_ref(), path))
    {
        Some(index) => ProtoRoot {
            index,
            usd: proto_path.clone(),
        },
        None => ProtoRoot::usd(proto_path),
    };
    // Adaptive subdivision refines a prototype by its size on screen only when
    // this is its one placement on the stage.
    let place =
        if caches.meshes.subdiv.adaptive.is_some() && caches.placement_count(&root.index) == 1 {
            ProtoPlace::Unshared(world_xf)
        } else {
            ProtoPlace::Shared
        };
    let parts = placed_parts(&root, caches, place);
    let first = world.count();
    let motion = super::index::data::motion_translate(&caches.prim_data(path));
    attach_proto_parts(
        world,
        &parts,
        world_xf,
        span.close.as_ref(),
        motion,
        "native instance",
    );
    debug!(
        "Instance {} uses prototype {proto_path} ({} instance(s), geom ids {first}..{})",
        path,
        parts.len(),
        world.count()
    );
}

/// Attaches every part of a prototype at `placement`, one instance each,
/// and returns how many kernel instances that was. A group's instance takes
/// one `geom_id` per slot, so the ids consumed can exceed the count.
/// Non-invertible placements are skipped: the kernel's instance transform
/// must be invertible, and a zero scale is a common "hide this instance"
/// idiom rather than an error.
///
/// `placement` is where the shutter opens; `close`, when the placement's
/// transform moves, where it closes. Each part ends the shutter at the span's
/// end with `part.local` folded in ([`Span::end`]), moved by `motion`, the
/// placing prim's `crust:motion:translate`.
fn attach_proto_parts(
    world: &mut WorldBuilder,
    parts: &[ProtoPart],
    placement: GMat4,
    close: Option<&GMat4>,
    motion: Option<Vec3>,
    what: &str,
) -> usize {
    let mut attached = 0;
    for part in parts {
        let xf = placement * part.local;
        let end = match (close, motion) {
            (None, None) => None,
            _ => Span {
                open: xf,
                close: close.map(|c| *c * part.local),
            }
            .end(motion),
        };
        if xf.determinant().abs() < 1e-12 {
            debug!("{what}: non-invertible instance transform — skipped");
            continue;
        }
        let xf = Affine3A::from_mat4(xf);
        // A leaf reports its own id; a group forwards its slot index on top
        // of the first of the consecutive ids its slots take.
        let label = if part.slots.len() > 1 {
            InstanceHitId::Offset(world.count() as u32)
        } else {
            InstanceHitId::Own
        };
        let geom_id = world.attach_labelled(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: xf,
                transform_end: end.map(Box::new),
            },
            part.slots[0].material.clone(),
            part.mask,
            label,
        );
        for slot in &part.slots[1..] {
            world.reserve_slot(slot.material.clone(), part.mask);
        }
        // `part.local` is already folded in, so this is the scale from the
        // prototype's own frame to world; each slot adds its own inside the
        // part. Geometry under *nested* instancer placements is the
        // exception: those scales live inside the committed kernel scene and
        // differ per placement, so it filters against the outer scale alone.
        // See the texture filtering gaps in openspec/specs/textures/design.md.
        let scale = placement_scale(&xf);
        for (i, slot) in part.slots.iter().enumerate() {
            let id = geom_id + i as u32;
            // An instance transforms the ray, not the triangles, so the
            // winding — and with it the barycentric order — is the
            // prototype's own: no swap.
            if let Some(map) = &slot.faces {
                world.set_face_map(id, map.clone(), false);
            }
            if let Some(map) = &slot.uvs {
                world.set_uv_map(id, map.clone(), false);
            }
            if let Some(table) = &slot.primvars {
                world.set_primvars(id, table.clone(), false);
            }
            match &slot.strands {
                Some(table) => world.set_strands(id, table.clone()),
                None if slot.curves => world.mark_curves(id),
                None => {}
            }
            world.set_placement_scale(id, scale * slot.scale);
        }
        attached += 1;
    }
    attached
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// The per-instance transform is USD's `translate ∘ orient ∘ scale`
/// (spec: scale first, then orientation, then position), composed under
/// the instancer's own world transform. `invisibleIds` prunes instances by
/// `ids`; where `ids` is absent the array index is the id, as USD
/// specifies.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
/// A `PointInstancer`'s prototypes and the placements that select them,
/// resolved once and reused by both the top-level emitter and the nested
/// (inside-a-prototype) path.
struct InstancerLayout {
    /// The `prototypes` relationship's ordered targets.
    pub(super) targets: Vec<sdf::Path>,
    /// Visible instances as `(index into targets, transform relative to
    /// the instancer)`. Instances hidden by `invisibleIds` are already
    /// dropped.
    pub(super) placements: Vec<(usize, GMat4)>,
    /// How many instances `invisibleIds` removed, for reporting.
    pub(super) hidden: usize,
}

/// The USD prims a point instancer places, in `instancerTopology/prototypes`
/// order: each `ForInstancer` copy named by the prim it copies
/// (`primOrigin/scenePath`).
pub(super) fn instancer_targets(prim: &PrimData) -> Vec<sdf::Path> {
    use moho::tokens::instancing as t;
    use moho::tokens::prim_origin as origin;
    let paths = |v: Option<sdf::Value>| match v {
        Some(sdf::Value::PathVec(v)) => v,
        _ => Vec::new(),
    };
    let copies = paths(
        prim.locate([t::INSTANCER_TOPOLOGY, t::PROTOTYPES])
            .and_then(|d| d.value()),
    );
    copies
        .iter()
        .map(|copy| {
            prim.index()
                .and_then(|index| {
                    paths(
                        PrimData::in_index(index, copy)
                            .locate([origin::PRIM_ORIGIN, origin::SCENE_PATH])
                            .and_then(|d| d.value()),
                    )
                    .into_iter()
                    .next()
                })
                .unwrap_or_else(|| copy.parent().unwrap_or_else(|| copy.clone()))
        })
        .collect()
}

/// Reads the instancer's Hydra data into placements. `None` when the prim
/// is not a usable instancer.
///
/// The prototypes are `instancerTopology/prototypes` (the `ForInstancer`
/// copies), each named by the USD prim it copies (`primOrigin/scenePath`);
/// which instance places which prototype is `instanceIndices`, and
/// `mask` hides the instances `invisibleIds` names. The transform is USD's
/// `translate ∘ orient ∘ scale` from the `hydra:instanceTranslations`,
/// `hydra:instanceRotations` and `hydra:instanceScales` primvars, in
/// instance order.
fn read_instancer(prim: &PrimData, report: bool) -> Option<InstancerLayout> {
    use moho::tokens::instancing as t;
    let targets = instancer_targets(prim);
    if targets.is_empty() {
        if report {
            warning!(
                InstancingNoPrototypes,
                at = prim.path(),
                "PointInstancer at {} has no `prototypes` targets — skipped",
                prim.path()
            );
        }
        return None;
    }

    // (instance, prototype), in instance order.
    let mut instances: Vec<(usize, usize)> = Vec::new();
    if let Some(indices) = prim
        .locate([t::INSTANCER_TOPOLOGY, t::INSTANCE_INDICES])
        .and_then(|d| d.as_vector().cloned())
    {
        for k in 0..indices.len() {
            let Some(list) = indices
                .get(k)
                .and_then(|d| d.value())
                .and_then(decode_i32_array)
            else {
                continue;
            };
            instances.extend(
                list.into_iter()
                    .filter_map(|i| usize::try_from(i).ok())
                    .map(|i| (i, k)),
            );
        }
    }
    instances.sort_unstable();
    if instances.is_empty() {
        if report {
            warning!(
                InstancingNoProtoIndices,
                at = prim.path(),
                "PointInstancer at {} places no instance (no `protoIndices`) — skipped",
                prim.path()
            );
        }
        return None;
    }
    let mask: Vec<bool> = match prim
        .locate([t::INSTANCER_TOPOLOGY, t::MASK])
        .and_then(|d| d.value())
    {
        Some(sdf::Value::BoolVec(v)) => v,
        _ => Vec::new(),
    };

    let primvar = |name: &str| prim.primvar(name).map(|p| p.value);
    let positions = primvar(t::INSTANCE_TRANSLATIONS)
        .and_then(decode_vec3f_array)
        .unwrap_or_default();
    let scales = primvar(t::INSTANCE_SCALES).and_then(decode_vec3f_array);
    let orientations = primvar(t::INSTANCE_ROTATIONS).and_then(orientations);

    let count = instances.last().map_or(0, |(i, _)| i + 1);
    if positions.len() < count && report {
        warning!(
            InstancingMissingPositions,
            at = prim.path(),
            "PointInstancer at {}: {count} protoIndices but only {} positions — extra instances \
             skipped",
            prim.path(),
            positions.len()
        );
    }

    let mut placements = Vec::with_capacity(instances.len());
    let mut hidden = 0usize;
    for (i, k) in instances {
        let Some(pos) = positions.get(i) else { break };
        if !mask.get(i).copied().unwrap_or(true) {
            hidden += 1;
            continue;
        }
        if k >= targets.len() {
            if report {
                warning!(
                    InstancingProtoIndexOutOfRange,
                    at = prim.path(),
                    "PointInstancer at {}: protoIndices[{i}] = {k} is out of range — instance \
                     skipped",
                    prim.path()
                );
            }
            continue;
        }

        let scale = scales
            .as_ref()
            .and_then(|s| s.get(i))
            .map_or(Vec3::ONE, |s| Vec3::new(s.x, s.y, s.z));
        let rotation = orientations
            .as_ref()
            .and_then(|q| q.get(i).copied())
            .unwrap_or(glam::Quat::IDENTITY);
        placements.push((
            k,
            GMat4::from_scale_rotation_translation(scale, rotation, Vec3::new(pos.x, pos.y, pos.z)),
        ));
    }

    Some(InstancerLayout {
        targets,
        placements,
        hidden,
    })
}

/// The prototype roots a point instancer at `at` places, one per target: its
/// `ForInstancer` copies (`instancerTopology/prototypes`), named by the
/// targets they copy, in the targets' order. A target inside a native
/// instance copies as an empty prim, as in UsdImaging 2.0.
fn proto_roots(caches: &ImportCaches<'_>, at: &sdf::Path, targets: &[sdf::Path]) -> Vec<ProtoRoot> {
    let copies = caches
        .inst
        .as_ref()
        .map(|inst| super::index::instancers::instancer_prototypes(inst.as_ref(), at))
        .unwrap_or_default();
    targets
        .iter()
        .enumerate()
        .map(|(k, target)| match copies.get(k) {
            Some(index) => ProtoRoot {
                index: index.clone(),
                usd: target.clone(),
            },
            None => ProtoRoot::usd(target),
        })
        .collect()
}

/// A prototype's parts, from the cache or freshly built.
fn prototype_parts(
    proto_path: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Arc<Vec<ProtoPart>> {
    let key = proto_path.key();
    if let Some(parts) = caches.protos.get(&key) {
        debug!(
            "Prototype {} ({}): reusing {} cached part(s)",
            proto_path.usd,
            key,
            parts.len()
        );
        return parts.clone();
    }
    let parts = Arc::new(build_parts(proto_path, caches, depth, ProtoPlace::Shared));
    caches.protos.insert(key, parts.clone());
    parts
}

/// A prototype's parts built for its one placement, uncached.
fn unshared_parts(
    proto_path: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
    world: GMat4,
) -> Vec<ProtoPart> {
    build_parts(proto_path, caches, depth, ProtoPlace::Unshared(world))
}

fn build_parts(
    root: &ProtoRoot,
    caches: &mut ImportCaches<'_>,
    depth: usize,
    place: ProtoPlace,
) -> Vec<ProtoPart> {
    let started = Instant::now();
    let proto_path = &root.usd;
    let parts = collect_proto_parts(root, caches, depth, place);
    if parts.is_empty() {
        warning!(
            InstancingEmptyPrototype,
            at = proto_path,
            "Prototype {proto_path} contributed no geometry"
        );
    } else {
        debug!(
            "Prototype {proto_path} ({}, nesting depth {depth}, {}): built {} part(s) in {:?}",
            root.index,
            match place {
                ProtoPlace::Shared => "shared",
                ProtoPlace::Unshared(_) => "placed once",
            },
            parts.len(),
            started.elapsed()
        );
    }
    parts
}

/// How many times an instancer at `frame` places each of its targets, counting
/// only placements that draw: a zero-scale placement (the "hide this instance"
/// idiom) is skipped at attachment, so it must not make a prototype shared.
fn target_counts(layout: &InstancerLayout, frame: &GMat4) -> Vec<usize> {
    let mut counts = vec![0usize; layout.targets.len()];
    for &(k, xf) in &layout.placements {
        if (*frame * xf).determinant().abs() >= 1e-12 {
            counts[k] += 1;
        }
    }
    counts
}

/// The drawn placements of each target of `instancer` at world transform
/// `world`, for [`count_placements`](super::count_placements): read quietly,
/// since the traversal reads the same instancer again and reports it then.
pub(super) fn instancer_target_counts(prim: &PrimData, world: &GMat4) -> Vec<(String, usize)> {
    let Some(layout) = read_instancer(prim, false) else {
        return Vec::new();
    };
    let counts = target_counts(&layout, world);
    layout
        .targets
        .iter()
        .map(ToString::to_string)
        .zip(counts)
        .collect()
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
pub(super) fn emit_point_instancer(
    world: &mut WorldBuilder,
    path: &sdf::Path,
    span: Span,
    caches: &mut ImportCaches<'_>,
) {
    let world_xf = span.open;
    let Some(layout) = read_instancer(&caches.prim_data(path), true) else {
        return;
    };
    // Per-instance primvars, and any above the instancer, do not reach the
    // prototypes' prims.
    caches
        .meshes
        .primvars
        .report_placement(&caches.prim_data(path), "the PointInstancer");
    let roots = proto_roots(caches, path, &layout.targets);
    // The version of its prototype each placement draws: the shared one, or —
    // in adaptive mode, for a target this instancer places once — one built
    // for that placement.
    // Counted across the whole stage (`count_placements`), so a
    // prototype two instancers each place once is shared, not built twice.
    let adaptive = caches.meshes.subdiv.adaptive.is_some();
    let counts: Vec<u32> = layout
        .targets
        .iter()
        .map(|t| {
            if adaptive {
                caches.placement_count(t)
            } else {
                0
            }
        })
        .collect();
    // A zero-scale placement (the "hide this instance" idiom) draws nothing:
    // `attach_proto_parts` would skip it, so no version is built for it.
    let drawn: Vec<bool> = layout
        .placements
        .iter()
        .map(|&(_, xf)| (world_xf * xf).determinant().abs() >= 1e-12)
        .collect();
    let placement_versions: Vec<(usize, Option<usize>)> = layout
        .placements
        .iter()
        .enumerate()
        .map(|(i, &(k, _))| (k, (adaptive && counts[k] == 1 && drawn[i]).then_some(i)))
        .collect();
    // As in `nested_instancer_parts`: every prototype in uniform mode, the
    // placed versions in adaptive mode, in (prototype, version) order.
    let versions: BTreeSet<(usize, Option<usize>)> = if adaptive {
        placement_versions
            .iter()
            .zip(&drawn)
            .filter(|&(_, &d)| d)
            .map(|(&v, _)| v)
            .collect()
    } else {
        (0..layout.targets.len()).map(|k| (k, None)).collect()
    };
    let proto_parts: BTreeMap<(usize, Option<usize>), Arc<Vec<ProtoPart>>> = versions
        .into_iter()
        .map(|(k, placed)| {
            let place = match placed {
                Some(i) => ProtoPlace::Unshared(world_xf * layout.placements[i].1),
                None => ProtoPlace::Shared,
            };
            ((k, placed), placed_parts(&roots[k], caches, place))
        })
        .collect();

    // A dense scatter can place millions of instances in this one call;
    // reserving the exact total up front avoids both the doubling-copy
    // cost and the over-allocation of growing the geometry table
    // incrementally (see `WorldBuilder::reserve`).
    let part_counts: BTreeMap<(usize, Option<usize>), usize> = proto_parts
        .iter()
        .map(|(&version, parts)| (version, parts.iter().map(|p| p.slots.len()).sum()))
        .collect();
    let total_geometries: usize = placement_versions
        .iter()
        .filter_map(|v| part_counts.get(v))
        .sum();
    world.reserve(total_geometries);

    let mut attached = 0usize;
    let first = world.count();
    for (&(_, xf), version) in layout.placements.iter().zip(&placement_versions) {
        // A hidden placement has no version in adaptive mode.
        let Some(parts) = proto_parts.get(version) else {
            continue;
        };
        let close = span.close.map(|c| c * xf);
        attached += attach_proto_parts(
            world,
            parts,
            world_xf * xf,
            close.as_ref(),
            None,
            "PointInstancer instance",
        );
    }

    debug!(
        "Imported PointInstancer at {} ({} instances of {} prototype(s): {} kernel instances attached, geom ids {first}..{}{})",
        path,
        layout.placements.len(),
        layout.targets.len(),
        attached,
        world.count(),
        if layout.hidden > 0 {
            format!(", {} hidden by invisibleIds", layout.hidden)
        } else {
            String::new()
        }
    );
}

/// Per-instance rotations, `hydra:instanceRotations` (`orientationsf`, or
/// the half-precision `orientations`, as UsdImaging picks them).
fn orientations(value: sdf::Value) -> Option<Vec<glam::Quat>> {
    let quat = |w: f32, x: f32, y: f32, z: f32| glam::Quat::from_xyzw(x, y, z, w).normalize();
    match value {
        sdf::Value::QuatfVec(v) => Some(v.iter().map(|q| quat(q.w, q.x, q.y, q.z)).collect()),
        sdf::Value::QuathVec(v) => Some(
            v.iter()
                .map(|q| quat(q.w.to_f32(), q.x.to_f32(), q.y.to_f32(), q.z.to_f32()))
                .collect(),
        ),
        _ => None,
    }
}
