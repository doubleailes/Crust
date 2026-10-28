//! Material binding and dispatch: `MaterialBindingAPI` resolution, the
//! per-stage material cache, and the decoders for `crust:openpbr`,
//! `PxrDisneyBsdf` and MaterialX references.

use crate::warning;
use moho::openusd;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3A;
use openusd::sdf;
use openusd::usd::Stage;
use tracing::debug;

use crate::color::Space;
use crate::material::{DispRemap, Displacement, DisplacementValue, Material, OpenPBR};

use super::ImportCaches;
use super::assets::{asset_value_path, load_ptex, load_uv_texture};
use super::attrs::decode_number;
use super::index::network::{Network, Node};
use super::preview::{preview_displacement, preview_surface_material};

/// Memoizes resolved materials by binding path (and shares one default),
/// so prims bound to the same USD material get pointer-identical Arcs —
/// which is what lets `MeshKey` recognize shared mesh geometry.
#[derive(Default)]
pub(super) struct MaterialCache {
    pub(super) by_path: HashMap<String, CachedMaterial>,
    /// While a material bound inside a re-rooted prototype is resolved, its
    /// name and the path the instancing chain holds it at ([`MatSource`]).
    pub(super) reading: Option<(sdf::Path, sdf::Path)>,
    pub(super) default: Option<Arc<dyn Material>>,
    /// Resolved `.ptx` path -> the opened texture, or `None` if it could not
    /// be opened. Keyed by filesystem path.
    pub(super) ptex: HashMap<(String, crate::ColorSpace), Option<Arc<dyn crate::PtexTexture>>>,
    /// `(resolved path, colour space)` -> the opened UV texture. Keyed by
    /// filesystem path for the same reason as `ptex`, and by colour space
    /// because the same file can legitimately be read both ways (a packed ORM
    /// map is raw; the albedo beside it is display-encoded) and the decode
    /// happens once, at load.
    pub(super) textures: HashMap<(String, crate::ColorSpace), Option<Arc<dyn crate::Texture2D>>>,
    /// The displacement of the `.mtlx` material the last resolution loaded,
    /// handed from [`load_mtlx_material`] to [`cached_material`], which keeps
    /// it with the material: the document is compiled once, for both.
    mtlx_displacement: Option<Arc<crate::materialx::MtlxDisplacement>>,
    /// The custom MaterialX node definitions under
    /// `PXR_MTLX_PLUGIN_SEARCH_PATHS` ([`mtlx_library`]), read once per import.
    pub(super) library: Arc<crust_mtlx::Library>,
}

/// The custom MaterialX node definitions in every `.mtlx` file under
/// `PXR_MTLX_PLUGIN_SEARCH_PATHS` (USD's `usdMtlx` variable), read for one
/// import: empty, and nothing read, when the variable is unset. A path or file
/// that cannot be read warns `mtlx.library_unreadable` and is skipped; the
/// rest of the library still loads.
pub(super) fn mtlx_library() -> Arc<crust_mtlx::Library> {
    let paths = &crate::config().mtlx_plugin_search_paths;
    if paths.is_empty() {
        return Arc::default();
    }
    let started = Instant::now();
    let (library, errors) = crust_mtlx::Library::load(paths);
    for (path, e) in errors {
        warning!(
            MtlxLibraryUnreadable,
            "PXR_MTLX_PLUGIN_SEARCH_PATHS: {} cannot be read ({e}) — its MaterialX node \
             definitions are not loaded",
            path.display()
        );
    }
    debug!(
        "MaterialX node library: {} nodedef(s), {} implemented as node graphs, from {} file(s) \
         under PXR_MTLX_PLUGIN_SEARCH_PATHS in {:?}",
        library.nodedefs().len(),
        library.implemented(),
        library.files.len(),
        started.elapsed()
    );
    Arc::new(library)
}

impl MaterialCache {
    /// An empty cache for one import, holding that import's MaterialX node
    /// library ([`mtlx_library`], read once by the import and kept in its
    /// provenance for the in-place rebuilds after it).
    pub(super) fn new(library: Arc<crust_mtlx::Library>) -> Self {
        MaterialCache {
            library,
            ..MaterialCache::default()
        }
    }

    /// `(prim read, material reading it)` for every material the cache
    /// resolved, prototype-scoped ones included ([`material_reads`]).
    pub(super) fn reads(&self) -> Vec<(String, String)> {
        let mut reads: Vec<(String, String)> = self
            .by_path
            .values()
            .flat_map(|cached| {
                cached
                    .reads
                    .iter()
                    .map(|r| (r.clone(), cached.name.clone()))
            })
            .collect();
        reads.sort();
        reads.dedup();
        reads
    }

    /// Each material prim the cache resolved → the `world` slots holding it
    /// and whether a mesh was displaced by it ([`crate::live::Provenance`]).
    /// By `Arc` identity: a slot holds exactly the `Arc` the cache handed out.
    /// Materials named under openusd's `/__Prototype_N` (renumbered per
    /// streamed stage) are left out: no edit names one, and two chunks'
    /// materials, cached apart under their propagated paths, can share one
    /// such name.
    pub(super) fn origins(
        &self,
        world: &crate::World,
    ) -> HashMap<String, crate::live::MaterialOrigin> {
        let mut by_arc: HashMap<*const (), &str> = HashMap::new();
        let mut out: HashMap<String, crate::live::MaterialOrigin> = HashMap::new();
        for cached in self.by_path.values() {
            let path = &cached.name;
            if path.starts_with("/__Prototype") {
                continue;
            }
            by_arc.insert(Arc::as_ptr(&cached.material) as *const (), path);
            let origin = out.entry(path.clone()).or_default();
            origin.displaced |= matches!(cached.displacement, Some(Some(_)));
        }
        for (slot, material) in world.materials().iter().enumerate() {
            if let Some(path) = by_arc.get(&(Arc::as_ptr(material) as *const ())) {
                out.get_mut(*path)
                    .expect("every path has an entry")
                    .slots
                    .push(slot as u32);
            }
        }
        out.retain(|_, o| !o.slots.is_empty());
        out
    }

    pub(super) fn default_material(&mut self) -> Arc<dyn Material> {
        self.default.get_or_insert_with(default_material).clone()
    }

    /// Cache key for a bound material path: the path itself, stable across a
    /// streamed import's stages (a material inside a prototype is read at
    /// Moho's propagated path, M11), so a material shared by several subtrees
    /// resolves to one `Arc`, which is what lets the mesh cache deduplicate
    /// across them.
    pub(super) fn key(&self, path: &str) -> String {
        path.to_string()
    }
}

/// A prim's resolved material and the displacement it defines, if any.
///
/// Cached together per material key ([`MaterialCache::key`]), the displacement
/// resolved the first time a mesh asks: displacement is a property of the
/// material, but it is consumed once at import rather
/// than per hit, so it lives beside the [`Material`] instead of inside it.
/// `displacement` is always `None` under `CRUST_DISPLACE=0`, and every
/// downstream step keys off its presence — so the off side is the code path
/// that existed before displacement, not an approximation of it.
#[derive(Clone)]
pub(super) struct BoundMaterial {
    pub(super) material: Arc<dyn Material>,
    pub(super) displacement: Option<Arc<Displacement>>,
}

/// One cached material, its displacement resolved on first demand.
///
/// Lazily, because only a mesh consumes a displacement: a material bound
/// only to spheres or curves never opens its displacement maps. A `.mtlx`
/// material's displacement program is compiled with the document, so it is
/// kept here until then.
pub(super) struct CachedMaterial {
    /// The material's name: its USD path, which the key differs from for a
    /// material read inside a re-rooted prototype.
    name: String,
    material: Arc<dyn Material>,
    /// The prims outside the material that its network reads (see
    /// [`material_reads`]).
    reads: Vec<String>,
    mtlx: Option<Arc<crate::materialx::MtlxDisplacement>>,
    /// `None` until a mesh asked; then the answer, which may be no
    /// displacement.
    displacement: Option<Option<Arc<Displacement>>>,
}

/// The binding purpose a final render resolves: USD's `full`, falling back to
/// the all-purpose binding (`bound_material` does the fallback).
/// `preview` is for interactive proxies and is never consulted.
pub(super) const RENDER_BINDING_PURPOSE: &str = "full";

/// The material bound to `prim` for a render, from the scene index's resolved
/// `materialBindings` (Moho's `MaterialBindingResolvingSceneIndex`): the
/// `full` purpose's, else the all-purpose one's — USD's `ComputeBoundMaterial`,
/// as `listing::bound_material` resolves it on the stage (the closest
/// binding, unless an ancestor's is `strongerThanDescendants`; a collection
/// binding including the prim over the direct one), answered
/// whether or not the target is a `Material` the walked stage holds.
pub(super) fn index_bound_material(prim: &super::index::data::PrimData) -> Option<sdf::Path> {
    use moho::tokens::material_bindings as t;
    [RENDER_BINDING_PURPOSE, ""]
        .into_iter()
        .find_map(|purpose| {
            match prim
                .locate([t::MATERIAL_BINDINGS, purpose, t::PATH])?
                .value()?
            {
                sdf::Value::PathVec(mut paths) if paths.len() == 1 => paths.pop(),
                _ => None,
            }
        })
}

/// The material bound to `prim`, for a prim that cannot be displaced
/// (spheres, curves): its displacement is not resolved.
pub(super) fn resolve_material(
    stage: &Stage,
    path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    let data = caches.prim_data(path);
    resolve_material_data(stage, &data, caches)
}

/// [`resolve_material`] for a prim read as `data` (a prototype's prim in the
/// instancing chain).
pub(super) fn resolve_material_data(
    stage: &Stage,
    data: &super::index::data::PrimData,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    match cached_material(stage, data, caches) {
        Some((key, _)) => caches.materials.by_path[&key].material.clone(),
        None => caches.materials.default_material(),
    }
}

/// The material bound to `prim`, with its displacement.
pub(super) fn resolve_bound(
    stage: &Stage,
    path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> BoundMaterial {
    let data = caches.prim_data(path);
    resolve_bound_data(stage, &data, caches)
}

/// [`resolve_bound`] for a prim read as `data` (a prototype's prim in the
/// instancing chain).
pub(super) fn resolve_bound_data(
    stage: &Stage,
    data: &super::index::data::PrimData,
    caches: &mut ImportCaches<'_>,
) -> BoundMaterial {
    let Some((key, mat_path)) = cached_material(stage, data, caches) else {
        return BoundMaterial {
            material: caches.materials.default_material(),
            displacement: None,
        };
    };
    let entry = &caches.materials.by_path[&key];
    let material = entry.material.clone();
    if let Some(displacement) = &entry.displacement {
        return BoundMaterial {
            material,
            displacement: displacement.clone(),
        };
    }
    let mtlx = entry.mtlx.clone();
    let displacement = if crate::config().displace {
        caches.materials.reading = re_rooted(data, &mat_path);
        let d = resolve_displacement(stage, &mat_path, mtlx, caches).map(Arc::new);
        caches.materials.reading = None;
        d
    } else {
        None
    };
    if let Some(d) = &displacement {
        debug!("Material {mat_path}: {d:?}");
    }
    if let Some(entry) = caches.materials.by_path.get_mut(&key) {
        entry.displacement = Some(displacement.clone());
    }
    BoundMaterial {
        material,
        displacement,
    }
}

/// Resolves and caches the material bound to `prim`, returning its cache key
/// and path; `None` for an unbound prim, which takes the default.
fn cached_material(
    stage: &Stage,
    data: &super::index::data::PrimData,
    caches: &mut ImportCaches<'_>,
) -> Option<(String, sdf::Path)> {
    let Some(target) = index_bound_material(data) else {
        debug!(
            "{} has no material binding — using default grey OpenPBR",
            data.path()
        );
        return None;
    };
    // A target the index re-rooted into a propagated prototype (Moho's M11)
    // is the material read, and keys the cache: stable across stages. It is
    // named, in warnings and provenance, by the USD prim it comes from.
    let mat_path = data.name_of(&target);
    let reading = re_rooted(data, &mat_path);
    let key = match &reading {
        Some((_, data_path)) => data_path.to_string(),
        None => caches.materials.key(mat_path.as_str()),
    };
    if caches.materials.by_path.contains_key(&key) {
        return Some((key, mat_path));
    }
    // Per *distinct* material, not per binding: a stage binding one material
    // to 10 000 prims logs this once.
    debug!("Resolving material {mat_path} ({key})");
    caches.materials.reading = reading;
    let (material, mtlx) = resolve_fresh(stage, &mat_path, caches);
    let reads = material_reads(stage, &mat_path, caches);
    caches.materials.reading = None;
    caches.materials.by_path.insert(
        key.clone(),
        CachedMaterial {
            name: mat_path.to_string(),
            material,
            reads,
            mtlx,
            displacement: None,
        },
    );
    Some((key, mat_path))
}

/// The prims outside the material at `mat_path` that its network reads: the
/// paths of the specs its prims compose from elsewhere (an `inherits`,
/// `specializes` or `references` arc's target, a referenced layer's prim),
/// read off `home`, the stage that composes it ([`stage_facts`]), and the
/// prims its connections reach, from the prims' generic data in the index
/// the material is read from ([`MatSource`]). An in-place edit of a prim one
/// of these names would change this material too ([`crate::live`]). A path
/// in another layer's namespace may coincide with an edited one; that costs a
/// full import, never a stale material.
///
/// [`stage_facts`]: super::index::stage_facts::foreign_spec_paths
fn material_reads(home: &Stage, mat_path: &sdf::Path, caches: &ImportCaches<'_>) -> Vec<String> {
    let src = MatSource::new(home, mat_path, caches);
    let mut reads = Vec::new();
    let mut paths = vec![src.prim.path().clone()];
    // Every node of every network: the shaders under the material (`__all`)
    // and those its connections reach outside it. Shaders are no prims of
    // the index (the material represents them), so the networks list them.
    for network in src.networks.values() {
        for name in network.nodes.keys() {
            let Some(node) = network.path(name) else {
                continue;
            };
            // Named where the material is: an instance proxy's nodes are
            // its prototype's, moved under the instance.
            let node = src.prim.name_of(&node);
            if node.has_prefix(mat_path) {
                paths.push(node);
            } else {
                reads.push(node.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    reads.extend(super::index::stage_facts::foreign_spec_paths(
        home, &paths, mat_path,
    ));
    reads.sort();
    reads.dedup();
    reads
}

/// For a material bound inside a re-rooted prototype, `(its name, the path
/// the index holds it at)`: what [`MatSource`] reads while it is resolved.
fn re_rooted(
    data: &super::index::data::PrimData,
    name: &sdf::Path,
) -> Option<(sdf::Path, sdf::Path)> {
    let target = index_bound_material(data)?;
    (target != *name).then(|| (name.clone(), target))
}

/// The material at `mat_path`, resolved without the cache, and the `.mtlx`
/// displacement program compiled with it: what a cache miss stores, and what
/// an in-place material update ([`super::live`]) rebuilds.
pub(super) fn resolve_fresh(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> (
    Arc<dyn Material>,
    Option<Arc<crate::materialx::MtlxDisplacement>>,
) {
    caches.materials.mtlx_displacement = None;
    let material = resolve_material_uncached(stage, mat_path, caches);
    (material, caches.materials.mtlx_displacement.take())
}

/// The scalar displacement a material defines, read from whichever of the
/// three authoring paths it uses: RenderMan's `PxrDisplace`, a MaterialX
/// `displacementshader`, or `UsdPreviewSurface.inputs:displacement`. `None`
/// when it defines none — the common case, which costs one child scan.
pub(super) fn resolve_displacement(
    stage: &Stage,
    mat_path: &sdf::Path,
    mtlx: Option<Arc<crate::materialx::MtlxDisplacement>>,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    let src = MatSource::new(stage, mat_path, caches);
    let found = if let Some((network, name)) = src.shader_with_id("PxrDisplace") {
        // The node as the `__all` network has it, reached by a terminal or
        // not.
        let (network, name) = (network.clone(), name.to_owned());
        pxr_displacement(&src, &network, &name, caches)
    } else if let Some(field) = mtlx {
        Some(Displacement::new(DisplacementValue::Field {
            field,
            scale: 1.0,
        }))
    } else {
        preview_displacement_of(&src, caches)
    }?;
    // A bound on the material, its constant primvar; a mesh prim's own
    // overrides it.
    let bound = src
        .prim
        .primvar("crust:displacementBound")
        .and_then(|p| p.f32());
    Some(found.with_authored_bound(bound))
}

/// A node parameter's number (`input_f32`).
fn param_f32(node: &super::index::network::Node, name: &str) -> Option<f32> {
    node.parameters
        .get(name)
        .and_then(|p| p.value.clone())
        .and_then(decode_number)
}

/// The node an input of `node` is connected to, with its name, when it is a
/// shader of the network (`upstream_shader`).
fn upstream<'n>(
    network: &'n super::index::network::Network,
    node: &str,
    name: &str,
) -> Option<(&'n str, &'n super::index::network::Node)> {
    let c = network.nodes.get(node)?.connections.get(name)?.first()?;
    network
        .nodes
        .get_key_value(&c.node)
        .map(|(k, v)| (k.as_str(), v))
}

/// RenderMan's `PxrDisplace`, read the way `PxrDisneyBsdf` is: off the
/// Material's interface where the network is wired to it.
///
/// The offset is `dispAmount · T(map)`: `dispAmount` authored on the shader or
/// connected to the interface (`inputs:dispScale` on the island);
/// `dispScalar` either a value or the chains the island authors:
/// `PxrPtexture → [PxrDispTransform] →`, its Ptex read raw from the
/// texture's `filename` (connected to `inputs:displacementMap`) and remapped
/// by the transform ([`DispRemap`]); or, as isDunesA's `soil` does, a
/// `PxrBlend` multiply (`operation = 18`) of two `PxrPtexture`s in place of
/// the one, their red channels multiplied. Any other network driving
/// `dispScalar` (another blend operation, a UV `PxrTexture`) is refused with a
/// warning, and so are `dispVector` / `modelDispVector`.
fn pxr_displacement(
    src: &MatSource,
    network: &super::index::network::Network,
    displace: &str,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    let mat_path = &src.path;
    let node = network.nodes.get(displace)?;
    for vector in ["dispVector", "modelDispVector"] {
        if node.connections.get(vector).is_some_and(|c| !c.is_empty()) {
            warning!(
                DisplacementVectorIgnored,
                at = mat_path,
                "Material {mat_path}: PxrDisplace.{vector} is vector displacement, which \
                 crust does not apply — ignored"
            );
        }
    }
    let amount = param_f32(node, "dispAmount").filter(|a| a.is_finite() && *a != 0.0)?;

    let Some((mut source, mut source_node)) = upstream(network, displace, "dispScalar") else {
        // A constant scalar: a uniform offset.
        let s = param_f32(node, "dispScalar").unwrap_or(0.0);
        let c = amount * s;
        return (c != 0.0 && c.is_finite())
            .then(|| Displacement::new(DisplacementValue::Constant(c)));
    };
    let mut remap = DispRemap::None;
    if source_node.id == "PxrDispTransform" {
        // RenderMan's defaults for anything unauthored.
        let mode = param_f32(source_node, "dispRemapMode").unwrap_or(0.0) as i32;
        let center = param_f32(source_node, "dispCenter").unwrap_or(0.5);
        let depth = param_f32(source_node, "dispDepth").unwrap_or(1.0);
        let height = param_f32(source_node, "dispHeight").unwrap_or(1.0);
        remap = match mode {
            1 => DispRemap::Centered { center },
            2 => DispRemap::DepthHeight {
                center,
                depth,
                height,
            },
            _ => DispRemap::None,
        };
        (source, source_node) = upstream(network, source, "dispScalar")?;
    }
    let id = |n: &super::index::network::Node| (!n.id.is_empty()).then(|| n.id.clone());
    let refuse = |name: &str, what: &super::index::network::Node| {
        let at = network
            .path(name)
            .map_or_else(|| name.to_owned(), |p| p.to_string());
        warning!(
            DisplacementUnevaluated,
            at = mat_path,
            "Material {mat_path}: PxrDisplace.dispScalar is driven by {at} ({:?}), which \
             crust does not evaluate — the surface is not displaced",
            id(what)
        );
    };
    let sources: Vec<(&str, &super::index::network::Node)> = match source_node.id.as_str() {
        "PxrPtexture" => vec![(source, source_node)],
        // RenderMan's `PxrBlend` operation 18 is multiply — the island also
        // uses it to mask its bump colour. `resultR` of the blend is the
        // product of the two inputs' red channels.
        "PxrBlend" if param_f32(source_node, "operation").map(|o| o as i32) == Some(18) => {
            let top = upstream(network, source, "topRGB");
            let bottom = upstream(network, source, "bottomRGB");
            match (top, bottom) {
                (Some(t), Some(b)) if [t.1, b.1].iter().all(|s| s.id == "PxrPtexture") => {
                    vec![t, b]
                }
                _ => {
                    refuse(source, source_node);
                    return None;
                }
            }
        }
        _ => {
            refuse(source, source_node);
            return None;
        }
    };
    let mut maps = Vec::with_capacity(sources.len());
    for (name, tex) in &sources {
        let node = network.path(name);
        let file = tex
            .parameters
            .get("filename")
            .and_then(|p| p.value.as_ref())
            .and_then(|v| {
                asset_value_path(v, caches.stage_path, &|| {
                    node.as_ref()
                        .and_then(|n| src.layer_dir(n, "inputs:filename"))
                })
            })?;
        maps.push(crate::PtexRef(load_ptex(
            &file,
            crate::ColorSpace::RAW,
            mat_path,
            caches,
        )?));
    }
    Some(Displacement::new(DisplacementValue::Ptex {
        maps,
        remap,
        scale: amount,
    }))
}

/// The preview surface's `inputs:displacement`, when the material shades
/// with a `UsdPreviewSurface`.
fn preview_displacement_of(src: &MatSource, caches: &mut ImportCaches<'_>) -> Option<Displacement> {
    let (network, node) =
        super::index::network::resolve(&src.networks, "surface", SURFACE_RENDER_CONTEXTS)?;
    let (shader, node) = node?;
    if node.id != "UsdPreviewSurface" {
        return None;
    }
    preview_displacement(src, network, shader, caches)
}

/// Render contexts `surface_source` is asked for, strongest first.
///
/// openusd 0.6 took no argument: it tried the universal terminal, then every
/// authored context in alphabetical order. 0.7 takes the preference list
/// explicitly and appends the universal context *last* unless it is named, so
/// the empty string leads here to keep a plain `outputs:surface` winning over
/// any render-specific one. `glslfx` is the preview context and the only
/// namespaced one crust can decode; an `ri` surface is a PxrDisneyBsdf, which
/// `has_shader_id` catches before this call is ever reached.
///
/// `mtlx` comes last: an inline MaterialX network (`ND_*` shaders, see
/// [`super::mtlx_network`]) is used when nothing the universal or preview
/// context names is decodable, so a stage that already rendered through its
/// preview surface renders exactly as before.
pub(super) const SURFACE_RENDER_CONTEXTS: &[&str] = &["", "glslfx", "mtlx"];

/// Render contexts for the `volume` terminal. Only a MaterialX network can
/// drive one (`ND_volume`, a VDF), so the universal terminal is read too, and
/// for a shader that is not MaterialX the terminal is ignored.
const VOLUME_RENDER_CONTEXTS: &[&str] = &["mtlx", ""];

fn resolve_material_uncached(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    use super::index::network::resolve;
    // A material whose whole definition is a reference into a `.mtlx` composes
    // to a prim with a `Material` type name and *nothing inside it*, because
    // openusd ships no MaterialX file-format plugin. So every schema query
    // below fails and the surface falls back to grey — which is exactly what
    // the MaterialX Teapot and Lion did before this existed.
    //
    // Consulted lazily, at each point where the USD path gives up, rather than
    // first: finding it means walking the prim's composition graph, and a
    // stage of ordinary USD materials should not pay for that.
    macro_rules! try_mtlx {
        () => {
            if let Some((file, node)) = super::index::stage_facts::mtlx_reference(stage, mat_path)
                && let Some(m) = load_mtlx_material(&file, &node, mat_path, caches)
            {
                debug!(
                    "Material {mat_path} resolved through the MaterialX reference {}</{node}>",
                    file.display()
                );
                return m;
            }
        };
    }

    // Which shader ids are MaterialX: the standard `ND_…`, and the ids the
    // import's node library defines.
    let library = caches.materials.library.clone();
    let src = MatSource::new(stage, mat_path, caches);
    if src.prim.ty() != moho::tokens::prim_type::MATERIAL {
        try_mtlx!();
        warning!(
            MaterialFallbackDefault,
            at = mat_path,
            "Material at {} not resolvable — using default grey OpenPBR",
            mat_path
        );
        return default_material();
    }

    // Checked before asking for the surface source, because that answer cannot
    // be trusted to name the network that matters. A material carrying several
    // render-context outputs — the Moana island authors `outputs:ri:surface`
    // (PxrDisneyBsdf), `outputs:glslfx:surface` (UsdPreviewSurface) and
    // `outputs:ri:displacement` on every prim — resolves through
    // `surface_source` to the *preview* shader, a stand-in for the GL
    // viewport that carries only a few of the material's values: the island
    // rendered uniformly pale and glossy instead of matte dark rock.
    if let Some((network, shader)) = src.shader_with_id("PxrDisneyBsdf") {
        debug!("Material {mat_path}: PxrDisneyBsdf child shader {shader}");
        return Arc::new(disney_to_openpbr(&src, network, shader, caches));
    }

    let is_mtlx = |id: &str| super::mtlx_network::is_mtlx_id(id, &library);
    // A `volume` terminal makes the material Typhoon's surface-volume
    // material: the volume is the medium inside the bound geometry, under the
    // MaterialX surface of the same network when there is one, and a
    // transparent medium boundary when there is none.
    let volume = resolve(&src.networks, "volume", VOLUME_RENDER_CONTEXTS)
        .and_then(|(network, node)| node.map(|(name, node)| (network, name, node)))
        .filter(|(_, _, node)| is_mtlx(&node.id));
    if let Some((volume_network, volume_name, _)) = volume {
        // Each context is checked for MaterialX on its own: an `mtlx`
        // surface that is not a MaterialX shader must not hide a universal
        // one that is.
        let mtlx_surface = |contexts: &[&str]| {
            resolve(&src.networks, "surface", contexts)
                .and_then(|(network, node)| node.map(|(name, node)| (network, name, node)))
                .filter(|(_, _, node)| is_mtlx(&node.id))
        };
        let surface = mtlx_surface(&["mtlx"]).or_else(|| mtlx_surface(&[""]));
        let other =
            resolve(&src.networks, "surface", SURFACE_RENDER_CONTEXTS).and_then(|(_, node)| node);
        if surface.is_none()
            && let Some((_, other)) = other
        {
            warning!(
                MaterialVolumeIgnored,
                at = mat_path,
                "Material {mat_path}: the volume terminal is MaterialX but the surface ({}) \
                 is not — the volume is ignored",
                other.id
            );
        } else {
            if let Some(m) = inline_mtlx_material(
                &src,
                surface.map(|(n, name, _)| (n, name)),
                Some((volume_network, volume_name)),
                caches,
            ) {
                return m;
            }
        }
    }

    // The surface terminal over the render contexts in preference order: the
    // first context that has one commits, and its node is the shader (none
    // when the terminal's source is no `Shader`, USD's invalid-shader result).
    let Some((network, Some((shader, node)))) =
        resolve(&src.networks, "surface", SURFACE_RENDER_CONTEXTS)
    else {
        // The MaterialX case reaches here when the material prim itself
        // composed (a wrapper layer `over`s it, so the prim exists) but
        // its shader network lives in the unreadable `.mtlx`.
        try_mtlx!();
        warning!(
            MaterialFallbackDefault,
            at = mat_path,
            "Material {} has no surface shader — using default grey OpenPBR",
            mat_path
        );
        return default_material();
    };

    let shader_id = (!node.id.is_empty()).then(|| node.id.clone());
    debug!("Material {mat_path}: surface shader id = {shader_id:?}");
    match shader_id.as_deref() {
        Some("crust:openpbr") => decode_crust_openpbr(
            node,
            &network
                .path(shader)
                .map(|p| p.to_string())
                .unwrap_or_default(),
            caches.working,
            caches.luma,
        ),
        Some("UsdPreviewSurface") => {
            // The preview surface may still be the Ptex-driven one — the Moana
            // island wires its `diffuseColor` to a Ptex node — so consult the
            // material's own interface input either way.
            preview_surface_material(&src, network, shader, caches)
        }
        Some("PxrDisneyBsdf") => Arc::new(disney_to_openpbr(&src, network, shader, caches)),
        Some(id) if is_mtlx(id) => {
            inline_mtlx_material(&src, Some((network, shader)), None, caches)
                .unwrap_or_else(default_material)
        }
        Some(other) => {
            warning!(
                MaterialFallbackDefault,
                at = mat_path,
                "Unrecognized shader id '{}' at {} — using default grey OpenPBR",
                other,
                mat_path
            );
            default_material()
        }
        None => {
            warning!(
                MaterialFallbackDefault,
                at = mat_path,
                "Shader at {} has no info:id — using default grey OpenPBR",
                mat_path
            );
            default_material()
        }
    }
}

/// Builds a material from an inline MaterialX network: the nodes driving
/// the `surface` and `volume` terminals, translated into one document
/// ([`super::mtlx_network`]) and compiled as a `.mtlx`'s would be. Textures
/// arrive as absolute paths, so they resolve against nothing further.
fn inline_mtlx_material(
    src: &MatSource,
    surface: Option<(&super::index::network::Network, &str)>,
    volume: Option<(&super::index::network::Network, &str)>,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn Material>> {
    let mat_path = &src.path;
    let net = super::mtlx_network::translate(
        caches.stage_path,
        &|p, name| src.layer_dir(p, name),
        &caches.materials.library,
        surface,
        volume,
    );
    if !net.reported.is_empty() {
        warning!(
            MtlxIncomplete,
            at = mat_path,
            "Material {mat_path}: MaterialX network not fully translated — {}",
            net.reported.join("; ")
        );
    }
    let surface = net.surface.as_deref().and_then(|n| net.doc.find_node(n));
    let volume = net.volume.as_deref().and_then(|n| net.doc.find_node(n));
    if surface.is_none() && volume.is_none() {
        return None;
    }
    let label = mat_path.as_str().to_string();
    compile_mtlx(
        &label,
        mat_path,
        std::path::Path::new("/"),
        caches,
        |host, luma| {
            crate::materialx::from_compiled(
                crust_mtlx::compile_terminals(&net.doc, surface, volume, host),
                luma,
            )
        },
    )
}

/// A material as the index readers read it: its prim and its networks
/// ([`super::index::network`]) in the scene index the import reads, and the
/// nodes' prims, for what a shader input authors itself.
pub(super) struct MatSource {
    /// The material's path, in the namespace it is bound in.
    pub(super) path: sdf::Path,
    pub(super) prim: super::index::data::PrimData,
    pub(super) networks: std::collections::BTreeMap<String, super::index::network::Network>,
    index: moho::scene_index::SceneIndexHandle,
    /// For a material inside a re-rooted prototype, `(the path the
    /// instancing chain holds it at, its name)`: its prims are read there
    /// and named here ([`super::index::data::PrimData::rebased`]).
    rebase: Option<(sdf::Path, sdf::Path)>,
    /// The stage that composes the material, for what is read off it: the
    /// layer an asset value is authored in ([`MatSource::layer_dir`]).
    stage: Stage,
}

impl MatSource {
    /// The material at `mat_path`, found on `home`.
    pub(super) fn new(home: &Stage, mat_path: &sdf::Path, caches: &ImportCaches<'_>) -> Self {
        use super::index::data::PrimData;
        // A material bound inside a re-rooted prototype: read in the
        // instancing chain, at the path the binding names (Moho's M11).
        if let Some((name, data)) = &caches.materials.reading
            && name == mat_path
            && let Some(inst) = &caches.inst
        {
            let prim = PrimData::rebased(inst, data, data, name);
            return MatSource {
                path: mat_path.clone(),
                networks: super::index::network::networks(&prim),
                prim,
                index: inst.clone(),
                rebase: Some((data.clone(), name.clone())),
                stage: home.clone(),
            };
        }
        // Read at the top of the import's chain, after the material-node
        // resolution (Moho's M10).
        let Some(index) = caches.top.clone() else {
            unreachable!("a material resolved outside a read of the scene index")
        };
        let prim = PrimData::in_index(&index, mat_path);
        MatSource {
            path: mat_path.clone(),
            networks: super::index::network::networks(&prim),
            prim,
            index,
            rebase: None,
            stage: home.clone(),
        }
    }

    /// The directory of the layer authoring the value of attribute `name` of
    /// the prim at `path` (read as the material is, so named by its USD
    /// path): where a relative asset path in it is anchored.
    pub(super) fn layer_dir(&self, path: &sdf::Path, name: &str) -> Option<std::path::PathBuf> {
        let attr = self.at(path).path().append_property(name).ok()?;
        super::index::stage_facts::authoring_layer_dir(&self.stage, &attr)
    }

    /// The prim at `path`, read as the material is.
    pub(super) fn at(&self, path: &sdf::Path) -> super::index::data::PrimData {
        use super::index::data::PrimData;
        match &self.rebase {
            Some((data, name)) => {
                let at = path
                    .as_str()
                    .strip_prefix(name.as_str())
                    .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                    .and_then(|rest| sdf::Path::new(format!("{data}{rest}").as_str()).ok())
                    .unwrap_or_else(|| path.clone());
                PrimData::rebased(&self.index, &at, data, name)
            }
            None => PrimData::in_index(&self.index, path),
        }
    }

    /// The shader under the material whose identifier (`info:id`, a token
    /// or a string) is `id`, with the network that holds it: the `__all`
    /// context, which lists every shader under a material, connected or not
    /// (`UsdImagingDataSourceMaterial`). Shaders are no prims of the index.
    fn shader_with_id(&self, id: &str) -> Option<(&super::index::network::Network, &str)> {
        let all = self.networks.get(moho::tokens::material::ALL)?;
        all.nodes
            .iter()
            .find(|(_, node)| node.id == id)
            .map(|(name, _)| (all, name.as_str()))
    }
}

/// `name` through the OCIO config, warning at `at` (an attribute's path) when
/// it does not know it: `attrs::named_space`.
pub(super) fn named_space(at: &str, name: &str) -> Option<Space> {
    let space = Space::named(name);
    if space.is_none() {
        warning!(
            ColorUnknownSpace,
            at = at,
            "{at}: colorSpace `{name}` is not defined by the OCIO config — the value is used as \
             authored"
        );
    }
    space
}

/// Node parameter `name`'s colour in the working space: authored in the
/// working space unless the parameter's `colorSpace` names another. `node`
/// is the node's stage path, for the warning about an unknown space.
pub(super) fn param_color(node: &Node, at: &str, name: &str, working: Space) -> Option<Vec3A> {
    let rgb = node.vec3(name)?;
    Some(
        match node
            .color_space(name)
            .and_then(|n| named_space(&format!("{at}.inputs:{name}"), n))
        {
            Some(space) => crate::color::convert(rgb, space, working),
            None => rgb,
        },
    )
}

pub(super) fn default_material() -> Arc<dyn Material> {
    Arc::new(OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)))
}

/// Maps RenderMan's `PxrDisneyBsdf` onto [`OpenPBR`].
///
/// The Moana island — the reason this exists — shades every surface with it,
/// so without this arm the whole dataset resolves to flat grey. Both models
/// descend from Burley's, which makes most of the mapping direct. The
/// parameters are the node's, as the material network carries them: the
/// island authors its values on the Material and connects the shader's
/// inputs to them, and Hydra gives each connected input the value of the
/// interface input it reads.
///
/// `baseColor` is read through the node it connects to: a `PxrColorCorrect`
/// (whose `inputRGB` holds the colour), and a `PxrPtexture` behind either
/// one, the material's Ptex ([`ptex_feeding`]).
///
/// Not mapped, because OpenPBR has no equivalent lobe: `subsurface*`,
/// `diffTrans`, `specularTint`, `scatter*`. Those surfaces render as
/// opaque dielectrics.
///
/// `sheen` is deliberately **not** mapped onto `fuzz_weight`, despite both
/// being "the retroreflective one". Disney's sheen is a small term *added* at
/// grazing angles; OpenPBR's fuzz is a layer *over* everything beneath it,
/// so at the island's authored `sheen = 1` the fuzz lobe (then a Charlie sheen
/// under a flat `1 − fuzz_weight`) replaced the base entirely — the lava rocks
/// lost their Ptex detail and rendered as smooth blue-grey plastic. A weight is not a weight just because it shares a
/// name, and there is no honest scalar between the two.
fn disney_to_openpbr(
    src: &MatSource,
    network: &Network,
    shader: &str,
    caches: &mut ImportCaches<'_>,
) -> OpenPBR {
    let mat_path = &src.path;
    let mut o = OpenPBR {
        luma: caches.luma,
        ..OpenPBR::default()
    };
    let Some(node) = network.nodes.get(shader) else {
        return o;
    };
    let f = |n: &str| node.f32(n);

    // `baseColor` reaches the BSDF through a `PxrColorCorrect` with gamma
    // 1/2.2, i.e. the authored value is display-encoded and the shader
    // decodes it to linear. Do the same, or every surface renders washed out.
    // `g22_rec709` — the plain power law, not the piecewise sRGB curve: it is
    // what that node applies, and matching the island's reference render
    // matters more here than matching the standard. A `colorSpace` the
    // parameter carries names another space instead.
    let (holder, holder_name, input) = match node.connection("baseColor") {
        Some(c) => match network.nodes.get(&c.node) {
            Some(cc) if cc.id == "PxrColorCorrect" => (cc, c.node.as_str(), "inputRGB"),
            _ => (node, shader, "baseColor"),
        },
        None => (node, shader, "baseColor"),
    };
    if let Some(rgb) = holder.vec3(input) {
        let at = network
            .path(holder_name)
            .map(|p| format!("{p}.inputs:{input}"))
            .unwrap_or_default();
        let source = holder
            .color_space(input)
            .and_then(|n| named_space(&at, n))
            .unwrap_or(Space::G22_REC709);
        o.base_color = crate::color::convert(rgb, source, caches.working);
    }
    if let Some(v) = f("metallic") {
        o.base_metalness = v;
    }
    if let Some(v) = f("roughness") {
        o.specular_roughness = v;
    }
    if let Some(v) = f("ior") {
        o.specular_ior = v;
    }
    if let Some(v) = f("anisotropic") {
        o.specular_roughness_anisotropy = v;
    }
    if let Some(v) = f("clearcoat") {
        o.coat_weight = v;
    }
    // Gloss is the complement of roughness.
    if let Some(v) = f("clearcoatGloss") {
        o.coat_roughness = (1.0 - v).clamp(0.0, 1.0);
    }
    if let Some(v) = f("specTrans") {
        o.transmission_weight = v;
    }
    // A cutout (`Material::opacity`), not transmission.
    if let Some(v) = f("opacity") {
        o.geometry_opacity = v;
    }
    if let Some(v) = f("isThin") {
        o.geometry_thin_walled = v != 0.0;
    }

    o.base_color_ptex = material_ptex(src, network, shader, "baseColor", caches);
    debug!(
        "PxrDisneyBsdf {mat_path}: baseColor={:?} roughness={} metalness={} fuzz={} coat={} \
         ior={} ptex={}",
        o.base_color,
        o.specular_roughness,
        o.base_metalness,
        o.fuzz_weight,
        o.coat_weight,
        o.specular_ior,
        o.base_color_ptex.is_some()
    );
    o
}

/// Parses a `.mtlx` and turns the named material node into a [`Material`].
///
/// Asset paths inside the document are relative to the document itself (the
/// MaterialX rule), not to the USD layer that referenced it, so texture
/// resolution anchors on `file`'s own directory.
fn load_mtlx_material(
    file: &std::path::Path,
    node: &str,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn Material>> {
    let dir = file.parent().unwrap_or(std::path::Path::new("."));
    let label = file.display().to_string();
    let library = caches.materials.library.clone();
    compile_mtlx(&label, mat_path, dir, caches, |host, luma| {
        crate::materialx::load_with(
            file,
            (!node.is_empty()).then_some(node),
            &library,
            host,
            luma,
        )
    })
}

/// Compiles a MaterialX material through `compile`, handing it a host whose
/// texture loader resolves `image` files against `dir` and whose colour
/// conversion targets the working space, with the working space's luminance
/// weights, and reports what the compiler could not represent. `label` names the material in the log: the
/// `.mtlx` file, or the USD material of an inline network. `mat_path` is the
/// USD material it resolves, which its warnings are recorded on.
fn compile_mtlx(
    label: &str,
    mat_path: &sdf::Path,
    dir: &std::path::Path,
    caches: &mut ImportCaches<'_>,
    compile: impl FnOnce(
        &crust_mtlx::Host<'_>,
        utils::Luma,
    ) -> Result<crate::materialx::Loaded, crate::materialx::MtlxError>,
) -> Option<Arc<dyn Material>> {
    // `RefCell` because the loader closure is called from inside the compiler
    // while `caches` would otherwise be mutably borrowed by the outer call.
    let (working, luma) = (caches.working, caches.luma);
    let cell = std::cell::RefCell::new(&mut *caches);
    let loader = |asset: &str, space: crust_mtlx::TexelSpace<'_>| -> Option<crate::TextureRef> {
        let mut c = cell.borrow_mut();
        load_uv_texture(
            &dir.join(asset),
            crate::ColorSpace::from_mtlx_image(space, working),
            mat_path,
            &mut c,
        )
        .map(crate::TextureRef)
    };
    // A literal colour with an effective `colorspace` is converted into the
    // working space once, at compile time; one with none is already in it.
    let convert = |space: &str, rgb: [f32; 3]| -> [f32; 3] {
        crate::ColorSpace::from_mtlx(Some(space), working)
            .resolved()
            .map_or(rgb, |s| s.decode_rgb(Vec3A::from_array(rgb)).to_array())
    };
    let host = crust_mtlx::Host {
        load_texture: &loader,
        convert_color: &convert,
    };
    // Billed as (total) minus (what the texture loads already billed): the
    // loader closure runs *inside* this call and adds its own decode time to
    // the same accumulator, so timing the whole thing on top of that would
    // count every texture twice — which showed up as a "Load assets" phase
    // reported at 178% of the parse phase that contains it.
    let started = Instant::now();
    let before = cell.borrow().asset_time;
    let loaded = compile(&host, luma);
    let nested = cell.borrow().asset_time - before;
    // `cell` is not used past this point, which ends its borrow of `caches`.
    caches.asset_time += started.elapsed().saturating_sub(nested);

    match loaded {
        Ok(l) => {
            if !l.unsupported.is_empty() {
                warning!(
                    MtlxIncomplete,
                    at = mat_path,
                    "MaterialX {label}: no operator for node type(s) {} — those inputs \
                     fall back to their defaults",
                    l.unsupported.join(", ")
                );
            }
            if !l.reported.is_empty() {
                warning!(
                    MtlxIncomplete,
                    at = mat_path,
                    "MaterialX {label}: not represented — {}",
                    l.reported.join("; ")
                );
            }
            debug!(
                "MaterialX {label} -> {} ({} textures resolved{})",
                l.summary,
                l.textures,
                if l.displacement.is_some() {
                    ", displaced"
                } else {
                    ""
                }
            );
            caches.materials.mtlx_displacement = l.displacement;
            Some(l.material)
        }
        Err(e) => {
            warning!(
                MtlxUnusable,
                at = mat_path,
                "MaterialX {label} not usable ({e}) — falling back"
            );
            None
        }
    }
}

/// The per-face colour texture a material binds, if any: the Ptex node
/// ([`ptex_feeding`]) input `input` of the node `shader` reads.
pub(super) fn material_ptex(
    src: &MatSource,
    network: &Network,
    shader: &str,
    input: &str,
    caches: &mut ImportCaches<'_>,
) -> Option<crate::PtexRef> {
    let (name, file) = ptex_feeding(network, shader, input)?;
    let node_path = network.path(name)?;
    let value = network.nodes.get(name)?.value(file)?;
    let path = asset_value_path(&value, caches.stage_path, &|| {
        src.layer_dir(&node_path, &format!("inputs:{file}"))
    })?;

    let space = crate::ColorSpace::new(Space::G22_REC709, caches.working);
    load_ptex(&path, space, &src.path, caches).map(crate::PtexRef)
}

/// The Ptex node input `input` of the node `shader` reads, with the name of
/// its file parameter: a `PxrPtexture` (`filename`) or the island's GL
/// preview `HwPtexTexture_1` (`file`), connected directly or through a
/// `PxrColorCorrect`'s `inputRGB`, as the Moana island wires its base
/// colour.
fn ptex_feeding<'n>(
    network: &'n Network,
    shader: &str,
    input: &str,
) -> Option<(&'n str, &'static str)> {
    let mut c = network.nodes.get(shader)?.connection(input)?;
    // A colour correction or two, at most, in the island's graphs; the bound
    // stops a cycle.
    for _ in 0..4 {
        let (name, node) = network.nodes.get_key_value(&c.node)?;
        match node.id.as_str() {
            "PxrPtexture" => return Some((name.as_str(), "filename")),
            "HwPtexTexture_1" => return Some((name.as_str(), "file")),
            "PxrColorCorrect" => c = node.connection("inputRGB")?,
            _ => return None,
        }
    }
    None
}

/// Decode a `crust:openpbr` shader into the OpenPBR material. Every input
/// name is camelCase mirror of the Rust snake_case, e.g. `base_color` →
/// `inputs:baseColor`, `subsurface_radius_scale` → `inputs:subsurfaceRadiusScale`.
///
/// Read off the node's parameters as the material network carries them.
fn decode_crust_openpbr(
    shader: &Node,
    shader_path: &str,
    working: Space,
    luma: utils::Luma,
) -> Arc<dyn Material> {
    let mut o = OpenPBR {
        luma,
        ..OpenPBR::default()
    };

    let f = |n: &str, d: f32| shader.f32(n).unwrap_or(d);
    // Every colour is authored in the working space unless its parameter's
    // `colorSpace` names another; `v` reads a vector that is not a colour
    // and is never converted.
    let c = |n: &str, d: Vec3A| param_color(shader, shader_path, n, working).unwrap_or(d);
    let v = |n: &str, d: Vec3A| shader.vec3(n).unwrap_or(d);
    let b = |n: &str, d: bool| shader.bool(n).unwrap_or(d);

    // Base
    o.base_weight = f("baseWeight", o.base_weight);
    o.base_color = c("baseColor", o.base_color);
    o.base_diffuse_roughness = f("baseDiffuseRoughness", o.base_diffuse_roughness);
    o.base_metalness = f("baseMetalness", o.base_metalness);

    // Specular
    o.specular_weight = f("specularWeight", o.specular_weight);
    o.specular_color = c("specularColor", o.specular_color);
    o.specular_roughness = f("specularRoughness", o.specular_roughness);
    o.specular_ior = f("specularIor", o.specular_ior);
    o.specular_roughness_anisotropy = f(
        "specularRoughnessAnisotropy",
        o.specular_roughness_anisotropy,
    );

    // Transmission
    o.transmission_weight = f("transmissionWeight", o.transmission_weight);
    o.transmission_color = c("transmissionColor", o.transmission_color);
    o.transmission_depth = f("transmissionDepth", o.transmission_depth);
    o.transmission_scatter = c("transmissionScatter", o.transmission_scatter);
    o.transmission_scatter_anisotropy = f(
        "transmissionScatterAnisotropy",
        o.transmission_scatter_anisotropy,
    );
    o.transmission_dispersion_scale = f(
        "transmissionDispersionScale",
        o.transmission_dispersion_scale,
    );
    o.transmission_dispersion_abbe_number = f(
        "transmissionDispersionAbbeNumber",
        o.transmission_dispersion_abbe_number,
    );

    // Subsurface
    o.subsurface_weight = f("subsurfaceWeight", o.subsurface_weight);
    o.subsurface_color = c("subsurfaceColor", o.subsurface_color);
    o.subsurface_radius = f("subsurfaceRadius", o.subsurface_radius);
    o.subsurface_radius_scale = v("subsurfaceRadiusScale", o.subsurface_radius_scale);
    o.subsurface_scatter_anisotropy = f(
        "subsurfaceScatterAnisotropy",
        o.subsurface_scatter_anisotropy,
    );

    // Fuzz
    o.fuzz_weight = f("fuzzWeight", o.fuzz_weight);
    o.fuzz_color = c("fuzzColor", o.fuzz_color);
    o.fuzz_roughness = f("fuzzRoughness", o.fuzz_roughness);

    // Coat
    o.coat_weight = f("coatWeight", o.coat_weight);
    o.coat_color = c("coatColor", o.coat_color);
    o.coat_roughness = f("coatRoughness", o.coat_roughness);
    o.coat_roughness_anisotropy = f("coatRoughnessAnisotropy", o.coat_roughness_anisotropy);
    o.coat_ior = f("coatIor", o.coat_ior);
    o.coat_darkening = f("coatDarkening", o.coat_darkening);

    // Thin film
    o.thin_film_weight = f("thinFilmWeight", o.thin_film_weight);
    o.thin_film_thickness = f("thinFilmThickness", o.thin_film_thickness);
    o.thin_film_ior = f("thinFilmIor", o.thin_film_ior);

    // Emission
    o.emission_luminance = f("emissionLuminance", o.emission_luminance);
    o.emission_color = c("emissionColor", o.emission_color);

    // Geometry
    o.geometry_opacity = f("geometryOpacity", o.geometry_opacity);
    o.geometry_thin_walled = b("geometryThinWalled", o.geometry_thin_walled);

    Arc::new(o)
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use crate::scene::usd_import::index::data::PrimData;
    use crate::scene::usd_import::index::input_chain;

    /// The scene index's resolved bindings agree with the direct resolution
    /// (`bound_material`) on every prim of `stage`, a purpose-binding sample
    /// and the collection, strength and nested-binding fixtures included.
    fn agree(stage: &Stage) -> usize {
        let input = input_chain(stage.clone(), None);
        let mut bound = 0;
        let mut stack = vec![sdf::Path::abs_root()];
        while let Some(path) = stack.pop() {
            // Moho's propagated prototypes and point-instancer copies have no
            // stage prim to compare with (and take Hydra's binding
            // inheritance, the change's second behaviour change).
            let rerooted = path
                .as_str()
                .contains(moho::tokens::instancing::NI_PROPAGATED_PROTOTYPES)
                || path.as_str().contains("/ForInstancer");
            if rerooted {
                continue;
            }
            if !path.is_abs_root() {
                let prim = super::super::prim_at(stage, path.clone());
                let direct = super::super::listing::bound_material(stage, &prim);
                let index = index_bound_material(&PrimData::in_index(&input.chain, &path));
                assert_eq!(direct, index, "{path}");
                bound += usize::from(direct.is_some());
            }
            stack.extend(input.chain.children(&path));
        }
        bound
    }

    fn open(text_or_sample: &str) -> Stage {
        let path = if text_or_sample.ends_with(".usda") {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../samples")
                .join(text_or_sample)
        } else {
            let dir = std::env::temp_dir().join("crust_binding_agreement");
            std::fs::create_dir_all(&dir).expect("temp dir");
            let path = dir.join(format!("{:016x}.usda", {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                text_or_sample.hash(&mut h);
                h.finish()
            }));
            std::fs::write(&path, text_or_sample).expect("write stage");
            path
        };
        super::super::stage_builder()
            .load(openusd::usd::InitialLoadSet::LoadAll)
            .open(path.to_str().expect("utf-8"))
            .expect("stage opens")
    }

    #[test]
    fn index_bindings_agree_with_the_direct_resolution() {
        for sample in [
            "purpose_bindings.usda",
            "cornellbox.usda",
            "instancing.usda",
            "nested_instancing.usda",
            "materialx_basic.usda",
            "render_passes.usda",
        ] {
            agree(&open(sample));
        }
        let fixture = r#"#usda 1.0
def Xform "W" (
    prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:set"]
)
{
    rel material:binding = </Looks/A>
    rel material:binding:full = </Looks/B> (bindMaterialAs = "strongerThanDescendants")
    rel material:binding:collection:pick = [</W.collection:set>, </Looks/C>]
    prepend rel collection:set:includes = </W/Picked>
    def Mesh "Plain" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding:full = </Looks/D>
    }
    def Mesh "Picked" {}
    def Xform "Deep" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </Looks/E>
        def Mesh "Leaf" {}
    }
}
def Scope "Looks"
{
    def Material "A" {}
    def Material "B" {}
    def Material "C" {}
    def Material "D" {}
    def Material "E" {}
}
"#;
        assert!(agree(&open(fixture)) > 0, "the fixture binds something");
    }
}
