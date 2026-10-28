//! In-place updates (design D9): what an update's batch changed, rebuilt
//! from the delegate's index by the functions the commit built it with, at
//! the index's time, and put into the committed scene. Never a second
//! implementation: the bit-identity of an in-place update with a fresh
//! commit rests on there being one. Anything a rebuild cannot vouch for is
//! refused ([`Structural`]), and the delegate commits afresh.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crust_rt::Geometry;
use glam::Affine3A;
use moho::openusd::sdf;
use moho::scene_index::SceneIndexHandle;

use crate::camera::Camera;
use crate::light::{LightKind, LightList};
use crate::live::{MeshKeyFacts, Provenance, ShutterKey, Structural};
use crate::material::Material;
use crate::rt_world::{GeometryEdit, World, WorldBuilder};
use crate::scene::Scene;
use crate::tracer::RenderSettings;
use crate::{AssetLoader, Warning};

use super::camera::build_camera;
use super::index::data::PrimData;
use super::index::host::HostFacts;
use super::light_links::LightLinks;
use super::materials::{resolve_displacement, resolve_fresh};
use super::mesh::{
    MeshKey, MeshNeeds, MeshPlace, SubdivPolicy, bake_indices, bake_normals, bake_verts,
    mesh_source, placement_scale,
};
use super::shapes::emit_sphere;
use super::xform::Span;
use super::{
    ImportCaches, ImportCtx, ValidOptions, apply_exposure, asks_about_time, emit_light, frame_pose,
    read_index,
};

/// What a rebuild reads: the delegate's index, the host's facts, the stage's
/// file and the asset loader, as the commit read them.
pub(in crate::scene) struct Rebuild<'a> {
    pub(in crate::scene) index: SceneIndexHandle,
    pub(in crate::scene) facts: &'a dyn HostFacts,
    pub(in crate::scene) path: &'a Path,
    pub(in crate::scene) assets: &'a dyn AssetLoader,
}

/// What an update changes, as far as an in-place update can follow it:
/// from the batch's dirtied locators ([`super::commit::classify`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::scene) struct Change {
    /// The `RenderSettings` prim the render uses, or the render camera.
    pub(in crate::scene) settings: bool,
    /// Material prims whose inputs changed.
    pub(in crate::scene) materials: Vec<String>,
    /// Light prims whose inputs changed.
    pub(in crate::scene) lights: Vec<String>,
    /// Lights of [`Change::lights`] whose geometry may have changed too (their
    /// transform, or an input that shapes an area light's emitter): the
    /// rebuild replaces their emitters' geometry
    /// ([`Provenance::light_geometry`]).
    pub(in crate::scene) light_geometry: Vec<String>,
    /// Prims whose transform changed, each moving the meshes it and its
    /// descendants placed ([`Provenance::movable`]).
    pub(in crate::scene) moved: Vec<String>,
    /// Mesh prims whose points, topology or primvars changed
    /// ([`Provenance::reshapable`]).
    pub(in crate::scene) reshaped: Vec<String>,
    /// The class is applied by committing afresh from the managed prims
    /// (a light added or removed, a receiver's categories), not in place.
    pub(in crate::scene) recommit: bool,
}

/// The settings a settings change re-reads.
pub(in crate::scene) struct SettingsUpdate {
    /// The stage's render settings, before any host override.
    pub(in crate::scene) settings: RenderSettings,
    pub(in crate::scene) camera: Camera,
    /// The products the stage asks for.
    pub(in crate::scene) aovs: crate::AovRequest,
    /// The camera prim `camera` was built from: the render camera, which a
    /// retarget moves to another prim.
    pub(in crate::scene) camera_path: String,
    /// The shutter as re-read, from that camera.
    pub(in crate::scene) shutter: ShutterKey,
    /// What the stamp records of the re-read camera and stage
    /// ([`Scene::source`]); the layer stack is the session's, unchanged.
    pub(in crate::scene) lens: Option<crate::scene::Lens>,
    pub(in crate::scene) motion_blur: bool,
    pub(in crate::scene) invisible_overrides: u32,
}

/// One rebuilt light: its new light-list entry and the materials of the
/// geometry it shares its `Emissive` with.
pub(in crate::scene) struct LightUpdate {
    pub(in crate::scene) index: usize,
    pub(in crate::scene) light: LightKind,
    /// Its per-lobe multipliers, which the light list keeps beside it.
    pub(in crate::scene) gains: crate::LobeGains,
    pub(in crate::scene) emitters: Vec<(u32, Arc<dyn Material>)>,
}

/// Rebuilt parts, ready to replace the scene's ([`Update::apply`]).
#[derive(Default)]
pub(in crate::scene) struct Update {
    pub(in crate::scene) settings: Option<SettingsUpdate>,
    /// Each rebuilt material and the slots it goes into.
    pub(in crate::scene) materials: Vec<(Vec<u32>, Arc<dyn Material>)>,
    pub(in crate::scene) lights: Vec<LightUpdate>,
    /// Geometry replaced in the world, which then commits its tree again.
    pub(in crate::scene) geometry: Vec<crate::rt_world::GeometryEdit>,
    /// The content keys reshaped meshes now have, by slot: written into the
    /// scene's provenance, so a later reshape compares against them.
    pub(in crate::scene) mesh_keys: Vec<(u32, MeshKeyFacts)>,
}

impl Update {
    /// Refuses an update the scene's world and lights cannot take without a
    /// re-import (see [`World::check_materials`], [`LightList::check_replace`]).
    pub(in crate::scene) fn check(
        &self,
        world: &World,
        lights: &LightList,
    ) -> Result<(), Structural> {
        world.check_geometry(&self.geometry)?;
        world.check_materials(&self.swaps())?;
        for l in &self.lights {
            lights.check_replace(l.index, &l.light)?;
        }
        Ok(())
    }

    fn swaps(&self) -> Vec<(&[u32], &Arc<dyn Material>)> {
        let mut swaps: Vec<(&[u32], &Arc<dyn Material>)> = self
            .materials
            .iter()
            .map(|(slots, m)| (slots.as_slice(), m))
            .collect();
        for l in &self.lights {
            for (id, m) in &l.emitters {
                swaps.push((std::slice::from_ref(id), m));
            }
        }
        swaps
    }

    /// Puts the rebuilt parts into `scene`, after [`Update::check`]: the
    /// materials into their slots, each light into its place (its emitter's
    /// slot with it, so the light and its geometry keep one `Emissive`), the
    /// settings, camera and products over the old. The light selection is
    /// the renderer's to rebuild (`Renderer::new` / `reconfigure`).
    pub(in crate::scene) fn apply(self, scene: &mut Scene) -> Result<(), Structural> {
        self.check(&scene.world, &scene.lights)?;
        scene.world.replace_materials(&self.swaps());
        if let Some(prov) = scene.provenance.as_mut() {
            for (slot, key) in self.mesh_keys {
                if let Some(s) = prov.mesh_slots.get_mut(slot as usize) {
                    s.key = Some(key);
                }
            }
        }
        if !self.geometry.is_empty() {
            scene.world.replace_geometry(self.geometry)?;
            let counters = &mut scene.stats.scene;
            counters.top_level = scene.world.primitive_breakdown().into();
            counters.unique = scene.world.unique_primitive_breakdown().into();
            counters.footprint = scene.world.memory_footprint();
            counters.primvar_tables_bytes = scene.world.primvar_table_bytes();
        }
        for l in self.lights {
            scene.lights.replace(l.index, l.light);
            scene.lights.set_lobe_gains(l.index, l.gains);
        }
        if let Some(s) = self.settings {
            // The camera rendered through may be another prim now: the
            // scene and its provenance name it, as a fresh import's would.
            if let Some(prov) = scene.provenance.as_mut() {
                prov.stage.camera = Some(s.camera_path.clone());
                prov.stage.shutter = s.shutter;
            }
            scene.camera_path = Some(s.camera_path);
            scene.settings = s.settings;
            scene.camera = s.camera;
            scene.aovs = s.aovs;
            scene.source.lens = s.lens;
            scene.source.depth_of_field = scene.camera.has_depth_of_field();
            scene.source.motion_blur = s.motion_blur;
            scene.source.invisible_overrides = Some(s.invisible_overrides);
            scene.stats.image = (&scene.settings).into();
        }
        Ok(())
    }
}

/// Warning codes a settings change can move, by family: the settings, the
/// products and their AOVs, the camera, the time code and the colour space.
/// Not `primvar.`: those are raised reading geometry, for the primvars the
/// geometry kept, and a refresh that would change that set is refused.
const SETTINGS_FAMILIES: &[&str] = &[
    "settings.",
    "product.",
    "aov.",
    "lpe.",
    "camera.",
    "time.",
    "color.",
    "subdiv.",
];

/// Rebuilds what `change` names from the delegate's index, as the commit
/// that built the scene would build it now, without touching the scene.
/// `warnings` are the scene's own: a rebuild about a prim they name is
/// refused. `world` is the scene's: a geometry rebuild reads the materials
/// and committed meshes it holds.
pub(in crate::scene) fn rebuild(
    prov: &Provenance,
    at: &Rebuild<'_>,
    warnings: &[Warning],
    change: &Change,
    world: &World,
) -> Result<Update, Structural> {
    let facts = &prov.stage;
    let working = facts
        .working
        .ok_or_else(|| Structural("the scene was not imported from a stage".into()))?;
    let mut update = Update::default();
    if change.settings {
        update.settings = Some(refresh_settings(prov, at, warnings)?);
    }
    let rebuilding = Rebuilding {
        prov,
        at,
        working,
        warnings,
    };
    for path in &change.materials {
        update.materials.push(rebuilding.material(path)?);
    }
    for path in &change.lights {
        let shaped = change.light_geometry.iter().any(|l| l == path);
        let (light, emitters) = rebuilding.light(path, shaped)?;
        update.lights.push(light);
        update.geometry.extend(emitters);
    }
    if !change.moved.is_empty() || !change.reshaped.is_empty() {
        let (edits, keys) = rebuilding.geometry(&change.moved, &change.reshaped, world)?;
        update.geometry.extend(edits);
        update.mesh_keys = keys;
    }
    Ok(update)
}

/// Whether `prim` is `root` or below it.
fn under(prim: &str, root: &str) -> bool {
    prim.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('.'))
}

/// Refuses a rebuild about `root` when the scene's warnings name it, or may
/// name it beyond the prims they keep: a fresh import could report them
/// differently now.
fn unwarned(warnings: &[Warning], root: &str) -> Result<(), Structural> {
    for w in warnings {
        if w.prims.len() >= crate::warnings::MAX_PRIMS || w.prims.iter().any(|p| under(p, root)) {
            return Err(Structural(format!(
                "the scene's {} warning may concern {root}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

/// Refuses a rebuild that raised a warning the scene does not already carry
/// for the same prim, or any about `root` or the whole stage.
fn same_warnings(raised: &[Warning], scene: &[Warning], root: &str) -> Result<(), Structural> {
    for w in raised {
        let known = !w.prims.is_empty()
            && w.prims.iter().all(|p| {
                !under(p, root)
                    && scene
                        .iter()
                        .any(|s| s.code == w.code && s.prims.iter().any(|q| q == p))
            });
        if !known {
            return Err(Structural(format!(
                "rebuilding {root} raised {}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

fn prim_path(path: &str) -> Result<sdf::Path, Structural> {
    sdf::Path::new(path)
        .ok()
        .filter(|p| p.is_prim())
        .ok_or_else(|| Structural(format!("{path} is not a prim path")))
}

/// A distinct mesh's slot and the content key it has now.
type SlotKey = (u32, MeshKeyFacts);

struct Rebuilding<'a> {
    prov: &'a Provenance,
    at: &'a Rebuild<'a>,
    working: crate::color::Space,
    warnings: &'a [Warning],
}

impl Rebuilding<'_> {
    fn caches(&self) -> ImportCaches<'_> {
        let mut caches = ImportCaches::new(
            self.at.assets,
            self.at.facts,
            self.at.path,
            SubdivPolicy::new(self.prov.stage.subdiv_level),
            self.working,
            self.prov.stage.mtlx_library.clone(),
        );
        caches.time = super::index::data::current_frame(self.at.index.as_ref());
        caches.scene = Some(self.at.index.clone());
        caches.top = Some(self.at.index.clone());
        caches.inst = Some(self.at.index.clone());
        caches
    }

    /// The material at `path`, resolved as the commit's first binding to
    /// it resolved it, into the slots it fills.
    fn material(&self, path: &str) -> Result<(Vec<u32>, Arc<dyn Material>), Structural> {
        let origin = self
            .prov
            .material(path)
            .ok_or_else(|| Structural(format!("the scene does not use {path}")))?;
        if origin.displaced {
            return Err(Structural(format!("{path} displaces a mesh")));
        }
        unwarned(self.warnings, path)?;
        let mat_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let mut caches = self.caches();
            let (material, mtlx) = resolve_fresh(&mat_path, &mut caches);
            let displaces = crate::config().displace
                && resolve_displacement(&mat_path, mtlx, &mut caches).is_some();
            if displaces {
                return Err(Structural(format!("{path} now defines a displacement")));
            }
            Ok(material)
        })();
        let raised = scope.finish();
        let material = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok((origin.slots.clone(), material))
    }

    /// The light at `path`, emitted as the traversal emitted it, its
    /// geometry ids the ones it had; with `shaped`, its emitters' geometry
    /// too, to replace the world's.
    fn light(
        &self,
        path: &str,
        shaped: bool,
    ) -> Result<(LightUpdate, Vec<GeometryEdit>), Structural> {
        let origin = self
            .prov
            .light(path)
            .ok_or_else(|| Structural(format!("the scene has no light {path}")))?;
        unwarned(self.warnings, path)?;
        let light_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            // The light's transform and inputs from the index, as the commit
            // reads them.
            let index = &self.at.index;
            let world_xf =
                crate::scene::hydra::xform::world(&PrimData::at(index.as_ref(), &light_path));
            let mut world = WorldBuilder::new();
            // Ids are handed out in attach order: claiming the ones before
            // the light's gives its geometry the ids it had.
            let filler: Arc<dyn Material> = crate::scene::hydra::materials::default_material();
            for _ in 0..origin.geoms.start {
                world.reserve_slot(filler.clone(), crate::ray::RayMask(0));
            }
            let mut ctx = scratch_ctx(self, world);
            let light = PrimData::in_index(index, &light_path);
            if !emit_light(&mut ctx, &light, world_xf) {
                return Err(Structural(format!("{path} is no longer a light")));
            }
            let geoms = origin.geoms.start..ctx.world.count() as u32;
            if ctx.lights.count() != 1 || geoms != origin.geoms {
                return Err(Structural(format!("{path} would emit other geometry")));
            }
            let shapes: Vec<GeometryEdit> = if shaped {
                geoms.clone().map(|id| ctx.world.edit_of(id)).collect()
            } else {
                Vec::new()
            };
            let emitters = geoms
                .map(|id| (id, ctx.world.material(id).clone()))
                .collect();
            let gains = ctx.lights.lobe_gains(0);
            let (light, _) = ctx.lights.remove(0);
            Ok((
                LightUpdate {
                    index: origin.index,
                    light,
                    gains,
                    emitters,
                },
                shapes,
            ))
        })();
        let raised = scope.finish();
        let light = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok(light)
    }
}

impl Rebuilding<'_> {
    /// The meshes the prims of `moved` and their descendants placed, placed
    /// again at their transforms now, and the meshes of `reshaped` read
    /// again: each by the import's own reading, interning and baking, so the
    /// geometry and side tables are the ones a fresh import gives.
    ///
    /// An instance keeps its committed mesh and takes the new transform. A
    /// baked mesh is read again and baked at the new transform; its key must
    /// be the one it had (a move does not change the mesh), or, reshaped,
    /// must equal no other distinct mesh's (a fresh import would then share
    /// one mesh between them and instance both).
    fn geometry(
        &self,
        moved: &[String],
        reshaped: &[String],
        world: &World,
    ) -> Result<(Vec<GeometryEdit>, Vec<SlotKey>), Structural> {
        let prov = self.prov;
        if !world.keeps_inputs() {
            return Err(Structural("the world kept no geometry to replace".into()));
        }
        if !prov.stage.primvars.is_empty() {
            return Err(Structural(
                "the products keep primvars the geometry would read again".into(),
            ));
        }
        // geom_id → (the root whose stage reads it, whether it is reshaped).
        let mut todo: BTreeMap<u32, (String, bool)> = BTreeMap::new();
        for root in moved {
            if let Some(why) = prov.movable(root) {
                return Err(Structural(why));
            }
            for ids in prov.geometry_of(root) {
                for id in ids.filter(|&id| {
                    prov.mesh(id).is_some() || prov.spheres.binary_search(&id).is_ok()
                }) {
                    todo.entry(id).or_insert_with(|| (root.clone(), false));
                }
            }
        }
        for prim in reshaped {
            if let Some(why) = prov.reshapable(prim) {
                return Err(Structural(why));
            }
            for ids in prov.geometry_of(prim) {
                for id in ids {
                    todo.entry(id).or_insert_with(|| (prim.clone(), true)).1 = true;
                }
            }
        }
        let mut edits = Vec::with_capacity(todo.len());
        // The keys this batch's reshapes give their slots, which later
        // reshapes in the batch compare against instead of the import's.
        let mut keys: Vec<SlotKey> = Vec::new();
        for (id, (_, reshape)) in todo {
            let prim_str = prov
                .prim_of_geometry(id)
                .ok_or_else(|| Structural(format!("geometry {id} has no prim")))?
                .to_owned();
            unwarned(self.warnings, &prim_str)?;
            let scope = crate::WarningScope::enter();
            let built = match prov.mesh(id) {
                Some(&origin) => self.placed_mesh(&prim_str, origin, reshape, world, &keys),
                None => self
                    .placed_sphere(&prim_str, id, world)
                    .map(|edit| (edit, None)),
            };
            let raised = scope.finish();
            let (edit, key) = built?;
            same_warnings(&raised, self.warnings, &prim_str)?;
            edits.push(edit);
            keys.extend(key);
        }
        Ok((edits, keys))
    }

    /// A static analytic sphere at its prim's transform on `stage`, placed
    /// by the traversal's own `emit_sphere` into a scratch world.
    fn placed_sphere(
        &self,
        prim_str: &str,
        id: u32,
        world: &World,
    ) -> Result<GeometryEdit, Structural> {
        let path = prim_path(prim_str)?;
        let index = &self.at.index;
        if !PrimData::at(index.as_ref(), &path).is_a("Sphere") {
            return Err(Structural(format!("{prim_str} is not a Sphere")));
        }
        let world_xf =
            crate::scene::hydra::xform::world(&PrimData::at(self.at.index.as_ref(), &path));
        // Emitted alone, at id 0: a sphere's geometry and tables do not
        // depend on its id, so the edit is re-addressed rather than the
        // scratch world padded up to the id, which made a batch of spheres
        // quadratic.
        let mut scratch = WorldBuilder::new();
        let data = PrimData::in_index(index, &path);
        let placed = emit_sphere(
            &mut scratch,
            &data,
            Span::fixed(world_xf),
            world.material_of(id).clone(),
            self.working,
        );
        if placed != Some(0) {
            return Err(Structural(format!(
                "{prim_str} would not be a static sphere"
            )));
        }
        let mut edit = scratch.edit_of(0);
        edit.geom_id = id;
        Ok(edit)
    }

    /// One placement of a direct mesh at its prim's transform on `stage`.
    fn placed_mesh(
        &self,
        prim_str: &str,
        origin: crate::live::MeshOrigin,
        reshape: bool,
        world: &World,
        batch: &[SlotKey],
    ) -> Result<(GeometryEdit, Option<SlotKey>), Structural> {
        let id = origin.geom_id;
        let path = prim_path(prim_str)?;
        let world_xf =
            crate::scene::hydra::xform::world(&PrimData::at(self.at.index.as_ref(), &path));
        // A singular placement is baked at attach time, with no slot: a
        // fresh import would build it another way.
        if world_xf.determinant().abs() < 1e-12 {
            return Err(Structural(format!(
                "{prim_str} would have a singular transform"
            )));
        }
        let l2w = Affine3A::from_mat4(world_xf);
        let scale = placement_scale(&l2w);
        if !origin.baked && !reshape {
            let Some(Geometry::Instance {
                scene,
                transform_end: None,
                ..
            }) = world.input(id)
            else {
                return Err(Structural(format!(
                    "geometry {id} is not a static instance"
                )));
            };
            let geometry = Geometry::Instance {
                scene: scene.clone(),
                transform: l2w,
                transform_end: None,
            };
            return Ok((world.edit_with(id, geometry, scale), None));
        }
        if !PrimData::at(self.at.index.as_ref(), &path).is_a("Mesh") {
            return Err(Structural(format!("{prim_str} is not a Mesh")));
        }
        let slot_facts = self
            .prov
            .mesh_slots
            .get(origin.slot as usize)
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh is unknown")))?;
        let old_key = slot_facts
            .key
            .as_ref()
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh is unknown")))?;
        let material = world.material_of(id).clone();
        let mut caches = self.caches();
        let meshes = &mut caches.meshes;
        let needs = MeshNeeds::of(&*material, None, &meshes.primvars, meshes.working);
        let data = PrimData::in_index(&self.at.index, &path);
        let src = mesh_source(
            &data,
            &data,
            needs,
            &mut meshes.subdiv,
            MeshPlace::World(&world_xf),
        )
        .ok_or_else(|| Structural(format!("{prim_str} no longer reads as a mesh")))?;
        let key = MeshKey::new(&src, &material);
        let key = MeshKeyFacts {
            geo_hash: key.geo_hash,
            n_points: key.n_points,
            n_indices: key.n_indices,
            n_uvs: key.n_uvs,
            smooth: key.smooth,
            material: old_key.material,
        };
        if !reshape && key != *old_key {
            return Err(Structural(format!("{prim_str}'s mesh changed too")));
        }
        // Every other slot's key as it is now: this batch's for a slot it
        // reshaped already, else the provenance's (which in-place reshapes
        // keep current, `Update::mesh_keys`).
        let current = |i: usize| {
            batch
                .iter()
                .find(|(slot, _)| *slot as usize == i)
                .map(|(_, k)| k)
                .or(self.prov.mesh_slots[i].key.as_ref())
        };
        if reshape
            && (0..self.prov.mesh_slots.len())
                .any(|i| i != origin.slot as usize && current(i) == Some(&key))
        {
            return Err(Structural(format!(
                "{prim_str}'s mesh would share another prim's"
            )));
        }
        let slot = meshes
            .intern(&data, src, &material, None)
            .ok_or_else(|| Structural(format!("{prim_str} produces no triangles")))?;
        let s = &mut meshes.slots[slot as usize];
        let geom = s
            .local
            .take()
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh was committed")))?;
        // As `flush_meshes` bakes a placement, tables and all.
        let mirrored = l2w.matrix3.determinant() < 0.0;
        let geometry = Geometry::TriangleMesh {
            vertices: bake_verts(&geom.verts, &l2w),
            indices: bake_indices(geom.tris, &l2w),
            normals: geom.normals.map(|ns| bake_normals(&ns, &l2w)),
        };
        let primvars = s.primvars.clone();
        let new_key = reshape.then_some((origin.slot, key));
        let edit = GeometryEdit {
            geom_id: id,
            geometry,
            map: s.faces.clone(),
            uv: s.uvs.clone(),
            shading: primvars.clone().filter(|t| t.shades()),
            swapped: mirrored,
            aov: primvars
                .filter(|t| t.channels.iter().any(Option::is_some))
                .map(|t| (t, mirrored)),
            scale,
        };
        Ok((edit, new_key))
    }
}

/// An import context around `world` with nothing in it but the caches a
/// fresh import starts with, for one light's emitter to run in.
fn scratch_ctx<'a>(r: &'a Rebuilding<'a>, world: WorldBuilder) -> ImportCtx<'a> {
    let mut lights = LightList::new();
    lights.set_luma(crate::color::luma(r.working));
    ImportCtx {
        world,
        lights,
        volumes: Vec::new(),
        camera: None,
        wanted_camera: None,
        first_camera: None,
        first_camera_path: None,
        cameras_seen: Vec::new(),
        caches: r.caches(),
        pending_meshes: Vec::new(),
        spheres: Vec::new(),
        links: LightLinks::default(),
        pass: None,
        light_origins: Vec::new(),
        settings: crate::tracer::RenderSettings::default(),
        framing: Default::default(),
        stage_path: r.at.path,
        assets: r.at.assets,
    }
}

/// The settings, products and camera re-read as the import reads them.
fn refresh_settings(
    prov: &Provenance,
    at: &Rebuild<'_>,
    warnings: &[Warning],
) -> Result<SettingsUpdate, Structural> {
    let facts = &prov.stage;
    if let Some(w) = warnings.iter().find(|w| {
        SETTINGS_FAMILIES
            .iter()
            .any(|f| w.code.as_str().starts_with(f))
    }) {
        return Err(Structural(format!(
            "the scene's {} warning may change with the settings",
            w.code.as_str()
        )));
    }
    let Some(rendered) = facts.camera.as_deref() else {
        return Err(Structural("the scene renders the procedural camera".into()));
    };
    let rendered = prim_path(rendered)?;
    let mut options = facts.options.clone();
    options.frame = super::index::data::current_frame(at.index.as_ref());
    let valid = ValidOptions::check(&options)
        .map_err(|e| Structural(format!("the import options: {e}")))?;
    let scope = crate::WarningScope::enter();
    let built = (|| {
        let read = read_index(at.facts, at.index.as_ref(), at.path, &options, &valid)
            .map_err(|e| Structural(format!("the settings: {e}")))?;
        if Some(read.working) != facts.working {
            return Err(Structural("the working colour space changed".into()));
        }
        if read.subdiv.level != facts.subdiv_level {
            return Err(Structural("the subdivision level changed".into()));
        }
        // Tessellation that reads the dicing camera stands only when it reads
        // the same camera, and what it read is what it would read now, bit
        // for bit: an exposure keeps it, a move, a lens or a resolution does
        // not. Moving a render camera that does not dice keeps it too.
        if read.dicing.as_ref().map(ToString::to_string) != facts.dicing {
            return Err(Structural("the dicing camera changed".into()));
        }
        if read.subdiv.adaptive.map(|r| r.key()) != facts.adaptive {
            return Err(Structural(
                "adaptive subdivision would read another camera projection".into(),
            ));
        }
        if facts.on_screen_shapes.is_some()
            && read.projection.map(|p| p.key()) != facts.on_screen_shapes
        {
            return Err(Structural(
                "implicit shapes would be sized for another camera projection".into(),
            ));
        }
        if facts.unsized_shapes && read.projection.is_some() {
            return Err(Structural(
                "implicit shapes would be sized for the newly named camera".into(),
            ));
        }
        // The camera a fresh import would render through: the one the
        // settings or the first product name, else the first camera the
        // traversal met. Another camera than the scene's is a retarget,
        // which stands only for a camera that import met (it would build it
        // when named), carrying no prim the scene imported (the rebuild
        // moves none). Which camera dices, and so whether a retarget moves
        // tessellation, is checked above through the keys.
        let target =
            match &read.wanted_camera {
                Some(choice) => choice.path().clone(),
                None => prim_path(facts.cameras.first().ok_or_else(|| {
                    Structural("the import met no camera to fall back to".into())
                })?)?,
            };
        let retarget = target != rendered;
        if retarget {
            let name = target.to_string();
            if !facts.cameras.contains(&name) {
                return Err(Structural(format!("{name} is not a camera the import met")));
            }
            if prov.any_below(&name) {
                return Err(Structural(format!(
                    "a prim the scene imported lies below {name}"
                )));
            }
        }
        // Geometry was placed at the shutter's ends, so another shutter
        // re-imports, as a transform edit does. A retarget reads it from the
        // new camera: it must open and close as the old one did.
        let shutter = read.shutter.key(
            asks_about_time(&read.shutter, &read.products),
            frame_pose(&read.shutter, &read.products),
        );
        let expected = if retarget {
            crate::live::ShutterKey {
                camera: Some(target.to_string()),
                ..facts.shutter.clone()
            }
        } else {
            facts.shutter.clone()
        };
        if shutter != expected {
            return Err(Structural("the camera shutter changed".into()));
        }
        if read.domes_seen_by_camera != facts.domes_seen_by_camera {
            return Err(Structural("the dome's camera visibility changed".into()));
        }
        // Display filters edited in place are a later change: a precomp,
        // before or after, re-imports.
        if facts.precomp || read.products.request.precomp.is_some() {
            return Err(Structural("the render settings author a precomp".into()));
        }
        // The geometry kept the primvars the products asked for when it was
        // read, and nothing else: asking for another needs it read again.
        if read.products.primvars.key() != facts.primvars {
            return Err(Structural(
                "the products ask for other primvars than the geometry kept".into(),
            ));
        }
        let invisible_overrides = at.facts.invisible_overrides().unwrap_or(0);
        let motion_blur = read.settings.motion_blur() && read.shutter.interval.is_some();
        let camera = target.to_string();
        let camera = camera.as_str();
        let rc = build_camera(
            at.index.as_ref(),
            at.facts,
            &target,
            &read.settings,
            read.products.framing,
        )
        .ok_or_else(|| Structural(format!("{camera} is no longer a camera")))?;
        rc.warn_lens_units(Some(camera));
        rc.warn_clipping(Some(camera));
        let settings = apply_exposure(
            read.settings,
            read.exposure_compensation,
            rc.exposure_scale,
            Some(camera),
        );
        Ok(SettingsUpdate {
            settings,
            camera: rc.camera,
            camera_path: camera.to_owned(),
            shutter,
            aovs: read.products.request,
            lens: rc.lens,
            motion_blur,
            invisible_overrides,
        })
    })();
    let raised = scope.finish();
    let update = built?;
    if let Some(w) = raised.first() {
        return Err(Structural(format!(
            "re-reading the settings raised {}",
            w.code.as_str()
        )));
    }
    Ok(update)
}
