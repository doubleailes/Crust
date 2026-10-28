//! In-place rebuilds ([`crate::live`]): one material, one light, or the
//! settings and camera, built again from the stage by the functions the
//! traversal built them with, at the import's time code. Never a second
//! implementation: the bit-identity of an in-place update with a fresh
//! import rests on there being one.
//!
//! Each rebuild composes only what it reads: a stage population-masked to
//! the material's or the light's prim with its payloads loaded, as one chunk
//! of the streaming import is, or the payload-free index stage for the
//! settings.

use moho::openusd;
use std::path::Path;
use std::sync::Arc;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Stage, StagePopulationMask};

use std::collections::{BTreeMap, HashMap};

use crust_rt::Geometry;
use glam::Affine3A;

use crate::light::LightList;
use crate::live::{
    LightUpdate, MeshKeyFacts, PrimFrame, Provenance, SettingsUpdate, Structural, Update,
};
use crate::material::Material;
use crate::rt_world::{GeometryEdit, World, WorldBuilder};
use crate::{AssetLoader, Warning};

use moho::scene_index::SceneIndexHandle;

use super::camera::build_camera;
use super::index::data::PrimData;
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
    open_stage, read_index, stage_builder,
};

/// The stage at `path`, payloads loaded, population-masked to `mask` (and
/// its paths' ancestors and descendants): what an in-place rebuild reads.
fn open_stage_masked(
    path: &Path,
    path_str: &str,
    mask: Vec<sdf::Path>,
) -> Result<Stage, crate::Error> {
    let open_error = |e: Box<dyn std::error::Error + Send + Sync>| crate::Error::UsdOpen {
        path: path.to_path_buf(),
        source: e,
    };
    let mask = StagePopulationMask::new(mask).map_err(|e| open_error(e.into()))?;
    stage_builder()
        .load(InitialLoadSet::LoadAll)
        .mask(mask)
        .open(path_str)
        .map_err(|e| open_error(e.into()))
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

pub(crate) fn rebuild(
    prov: &Provenance,
    layer: &Path,
    assets: &dyn AssetLoader,
    warnings: &[Warning],
    change: &crate::live::Change,
    world: &World,
) -> Result<Update, Structural> {
    let facts = &prov.stage;
    let path_str = layer
        .to_str()
        .ok_or_else(|| Structural("the layer path is not UTF-8".into()))?;
    let working = facts
        .working
        .ok_or_else(|| Structural("the scene was not imported from a stage".into()))?;
    let mut update = Update::default();
    if change.settings {
        update.settings = Some(refresh_settings(prov, layer, path_str, warnings)?);
    }
    let rebuilding = Rebuilding {
        prov,
        layer,
        path_str,
        working,
        assets,
        warnings,
    };
    for path in &change.materials {
        update
            .materials
            .push(rebuilding.material(path, &change.shaders)?);
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

/// Refuses a material whose network reads a prim outside it: a stage masked
/// to the material does not compose it, where the import's stage did. Read
/// from the generic connections of the material's prims in `index`.
fn closed_network(index: &SceneIndexHandle, root: &sdf::Path) -> Result<(), Structural> {
    let mut stack = vec![PrimData::in_index(index, root)];
    while let Some(prim) = stack.pop() {
        for name in prim.attribute_names() {
            for target in prim.connections(&name) {
                if !target.prim_path().has_prefix(root) {
                    return Err(Structural(format!(
                        "{} connects to {target}, outside {root}",
                        prim.attribute_path(&name)
                    )));
                }
            }
        }
        stack.extend(prim.children());
    }
    Ok(())
}

/// Whether `path` is a `Shader` prim in `index`: its USD type,
/// `__usdPrimInfo/typeName`.
fn is_shader(index: &SceneIndexHandle, path: &sdf::Path) -> bool {
    PrimData::at(index.as_ref(), path).usd_type() == "Shader"
}

/// A distinct mesh's slot and the content key it has now.
type SlotKey = (u32, MeshKeyFacts);

struct Rebuilding<'a> {
    prov: &'a Provenance,
    layer: &'a Path,
    path_str: &'a str,
    working: crate::color::Space,
    assets: &'a dyn AssetLoader,
    warnings: &'a [Warning],
}

impl Rebuilding<'_> {
    fn caches(&self) -> ImportCaches<'_> {
        let mut caches = ImportCaches::new(
            self.assets,
            self.layer,
            SubdivPolicy::new(self.prov.stage.subdiv_level),
            self.working,
            self.prov.stage.mtlx_library.clone(),
        );
        caches.time = self.prov.stage.options.frame;
        caches
    }

    /// A scene index over `stage`, at the import's time, for the readers
    /// that have moved onto it: the same readers as the import's.
    fn scene(&self, stage: &Stage) -> super::index::Input {
        super::index::input_chain(stage.clone(), self.prov.stage.options.frame)
    }

    fn masked(&self, path: &sdf::Path) -> Result<Stage, Structural> {
        open_stage_masked(self.layer, self.path_str, vec![path.clone()])
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))
    }

    /// The material at `path`, resolved as the traversal's first binding to
    /// it resolved it, into the slots it fills. Each of `shaders` under it
    /// must be a `Shader` on the material's stage ([`Change::shaders`]).
    ///
    /// [`Change::shaders`]: crate::live::Change::shaders
    fn material(
        &self,
        path: &str,
        shaders: &[String],
    ) -> Result<(Vec<u32>, Arc<dyn Material>), Structural> {
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
            let stage = self.masked(&mat_path)?;
            let scene = self.scene(&stage);
            closed_network(&scene.chain, &mat_path)?;
            for shader in shaders {
                let shader = prim_path(shader)?;
                if shader.has_prefix(&mat_path) && !is_shader(&scene.chain, &shader) {
                    return Err(Structural(format!("{shader} is not a shader of {path}")));
                }
            }
            let mut caches = self.caches();
            caches.top = Some(scene.chain.clone());
            let (material, mtlx) = resolve_fresh(&stage, &mat_path, &mut caches);
            let displaces = crate::config().displace
                && resolve_displacement(&stage, &mat_path, mtlx, &mut caches).is_some();
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
            let stage = self.masked(&light_path)?;
            // The light's transform and inputs from an index over the masked
            // stage, as the import reads them.
            let scene = self.scene(&stage);
            let world_xf = super::xform::world(&PrimData::at(scene.chain.as_ref(), &light_path));
            let mut world = WorldBuilder::new();
            // Ids are handed out in attach order: claiming the ones before
            // the light's gives its geometry the ids it had.
            let filler: Arc<dyn Material> = super::materials::default_material();
            for _ in 0..origin.geoms.start {
                world.reserve_slot(filler.clone(), crate::ray::RayMask(0));
            }
            let mut ctx = scratch_ctx(self, world);
            let light = PrimData::in_index(&scene.chain, &light_path);
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
        let mut stages: HashMap<String, Stage> = HashMap::new();
        let mut edits = Vec::with_capacity(todo.len());
        // The keys this batch's reshapes give their slots, which later
        // reshapes in the batch compare against instead of the import's.
        let mut keys: Vec<SlotKey> = Vec::new();
        for (id, (root, reshape)) in todo {
            let prim_str = prov
                .prim_of_geometry(id)
                .ok_or_else(|| Structural(format!("geometry {id} has no prim")))?
                .to_owned();
            unwarned(self.warnings, &prim_str)?;
            if !stages.contains_key(&root) {
                let stage = self.masked(&prim_path(&root)?)?;
                stages.insert(root.clone(), stage);
            }
            let stage = &stages[&root];
            let scope = crate::WarningScope::enter();
            let built = match prov.mesh(id) {
                Some(&origin) => self.placed_mesh(stage, &prim_str, origin, reshape, world, &keys),
                None => self
                    .placed_sphere(stage, &prim_str, id, world)
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
        stage: &Stage,
        prim_str: &str,
        id: u32,
        world: &World,
    ) -> Result<GeometryEdit, Structural> {
        let path = prim_path(prim_str)?;
        let scene = self.scene(stage);
        if !PrimData::at(scene.chain.as_ref(), &path).is_a("Sphere") {
            return Err(Structural(format!("{prim_str} is not a Sphere")));
        }
        let world_xf =
            super::xform::stage_local_to_world(stage, &path, self.prov.stage.options.frame);
        // Emitted alone, at id 0: a sphere's geometry and tables do not
        // depend on its id, so the edit is re-addressed rather than the
        // scratch world padded up to the id, which made a batch of spheres
        // quadratic.
        let mut scratch = WorldBuilder::new();
        let data = super::index::data::PrimData::in_index(&scene.chain, &path);
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
        stage: &Stage,
        prim_str: &str,
        origin: crate::live::MeshOrigin,
        reshape: bool,
        world: &World,
        batch: &[SlotKey],
    ) -> Result<(GeometryEdit, Option<SlotKey>), Structural> {
        let id = origin.geom_id;
        let path = prim_path(prim_str)?;
        let world_xf =
            super::xform::stage_local_to_world(stage, &path, self.prov.stage.options.frame);
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
        if !PrimData::at(self.scene(stage).chain.as_ref(), &path).is_a("Mesh") {
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
        let scene = self.scene(stage);
        let data = super::index::data::PrimData::in_index(&scene.chain, &path);
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
        cameras_seen: Vec::new(),
        caches: r.caches(),
        pending_meshes: Vec::new(),
        spheres: Vec::new(),
        arc_sources: None,
        links: LightLinks::default(),
        pass: None,
        light_origins: Vec::new(),
        settings: crate::tracer::RenderSettings::default(),
        framing: Default::default(),
        stage_path: r.layer,
        assets: r.assets,
        layers: Default::default(),
    }
}

/// The settings, products and camera re-read as the import reads them.
fn refresh_settings(
    prov: &Provenance,
    layer: &Path,
    path_str: &str,
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
    let options = ValidOptions::check(&facts.options)
        .map_err(|e| Structural(format!("the import options: {e}")))?;
    let scope = crate::WarningScope::enter();
    let built = (|| {
        let index = open_stage(layer, path_str)
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
        let scene = super::index::input_chain(index.clone(), facts.options.frame);
        let read = read_index(
            &index,
            scene.chain.as_ref(),
            layer,
            &facts.options,
            &options,
        )
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
        let invisible_overrides = super::index::stage_facts::invisible_overrides(&index);
        let motion_blur = read.settings.motion_blur() && read.shutter.interval.is_some();
        drop(index);
        let camera = target.to_string();
        let camera = camera.as_str();
        let stage = open_stage_masked(layer, path_str, vec![target.clone()])
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
        let scene = super::index::input_chain(stage.clone(), facts.options.frame);
        let rc = build_camera(
            scene.chain.as_ref(),
            &stage,
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

/// Each of `paths` as the import composes it: on a stage masked to them with
/// payloads loaded (so ops a payload authors count), at the import's time
/// code, composed from a scene index over that stage as the walk composes
/// (`xform::stage_local_to_world`), with the ops a placement edits read in
/// `stage_facts` ([`super::index::stage_facts::authored_ops`]).
pub(crate) fn prim_frames(
    prov: &Provenance,
    layer: &Path,
    paths: &[String],
    place_op: &str,
) -> Result<Vec<PrimFrame>, Structural> {
    let path_str = layer
        .to_str()
        .ok_or_else(|| Structural("the layer path is not UTF-8".into()))?;
    let prims = paths
        .iter()
        .map(|p| prim_path(p))
        .collect::<Result<Vec<_>, _>>()?;
    let stage = open_stage_masked(layer, path_str, prims.clone())
        .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
    // Warnings the walk raises are the import's to report, not placement's.
    let _scope = crate::WarningScope::enter();
    prims
        .iter()
        .map(|path| {
            let ops = super::index::stage_facts::authored_ops(&stage, path, place_op)
                .ok_or_else(|| Structural(format!("{path} is not a prim of the stage")))?;
            let parent = path
                .parent()
                .map(|p| super::xform::stage_local_to_world(&stage, &p, prov.stage.options.frame))
                .unwrap_or(glam::Mat4::IDENTITY);
            let world = super::xform::stage_local_to_world(&stage, path, prov.stage.options.frame);
            Ok(PrimFrame {
                path: path.to_string(),
                xformable: ops.xformable,
                resets_xform_stack: ops.order.iter().any(|e| e == "!resetXformStack!"),
                parent_to_world: parent.as_dmat4(),
                local_to_world: world.as_dmat4(),
                xform_op_order: ops.order,
                place_op: ops.place,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-place rebuild resolves against the node library the import
    /// read, the very `Arc`: it reads no `.mtlx` file again, so it cannot
    /// raise the import's `mtlx.library_unreadable` a second time.
    #[test]
    fn a_rebuild_reuses_the_imports_node_library() {
        let mut library = crust_mtlx::Library::default();
        library
            .merge_text(r#"<materialx><nodedef name="ND_k" node="k"><output name="out" type="float" /></nodedef></materialx>"#)
            .expect("library parses");
        let mut prov = Provenance::default();
        prov.stage.mtlx_library = Arc::new(library);
        let rebuilding = Rebuilding {
            prov: &prov,
            layer: Path::new("unused.usda"),
            path_str: "unused.usda",
            working: crate::color::Space::LIN_REC709,
            assets: &crate::NoAssets,
            warnings: &[],
        };
        let caches = rebuilding.caches();
        assert!(Arc::ptr_eq(
            &caches.materials.library,
            &prov.stage.mtlx_library
        ));
    }
}
