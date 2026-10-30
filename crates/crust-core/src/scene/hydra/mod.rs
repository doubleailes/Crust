//! The delegate's scene side (design D1): reads the scene index a host hands
//! it (Moho's standard chain over the host's stage, and what only the host
//! knows, [`index::host::HostFacts`]) into a runtime `Scene`
//! (camera, world, lights, render settings). The batch host is
//! [`crate::scene::usd_import`].
//!
//! # Layout
//!
//! This module owns the commit as a whole — [`commit_scene`] and the state threaded
//! through it ([`ImportCtx`], [`ImportCaches`]). Every reader reads Moho's
//! scene index ([`index`]: the renderer filters, the prim data, the material
//! networks, the instancers, and the host's facts). The commit ([`commit`])
//! visits the managed prims in path order and dispatches each, by its Hydra
//! type, to its family's reader:
//!
//! | module         | index reader of                                              |
//! |----------------|--------------------------------------------------------------|
//! | [`settings`]   | `renderSettings`: its `namespacedSettings` (`crust:*` render attributes), camera choice |
//! | [`products`]   | `renderProduct` / `renderVar`: outputs, resolution, AOVs      |
//! | [`precomp`]    | `crust:precomp` sample-filter graphs                          |
//! | [`passes`]     | the active `renderPass` and its collections (`matte`, `cameraVisibility`) |
//! | [`attrs`]      | the decoders every reader shares; subdivision and adaptive levels |
//! | [`xform`]      | the local `xform` → world matrices, motion over the shutter   |
//! | [`camera`]     | `camera`                                                      |
//! | [`mesh`]       | `mesh`: interning, deferred bake-vs-instance, side tables    |
//! | [`adaptive`]   | the camera's projection; adaptive subdivision: a mesh's level from its size on screen |
//! | [`shapes`]     | `sphere`, `basisCurves`, `nurbsCurves`                        |
//! | [`strands`]    | curve primvars kept for shading                               |
//! | [`implicit`]   | `cube`, `plane`, `cylinder`, `cone`, `capsule` (+ `_1`) → GeomUtil meshes |
//! | [`instancing`] | `instancer`: Moho's propagated prototypes and point-instancer copies |
//! | [`lights`]     | every light type, shaping and IES                            |
//! | [`light_links`] | the lights' `lightLink` / `shadowLink` and the receivers' `categories`, installed after the commit |
//! | [`materials`]  | `materialBindings`, the material cache, shader dispatch       |
//! | [`preview`]    | `UsdPreviewSurface` + `UsdUVTexture` networks                |
//! | [`mtlx_network`] | inline MaterialX (`ND_*`) networks → crust-mtlx documents  |
//! | [`assets`]     | textures, Ptex and IES files, through the host's loader       |
//! | [`volume`]     | `primvars:crust:volume:*` regions                                     |
//!
//! Submodules expose what their siblings and the host need as
//! `pub(in crate::scene)` and import each other explicitly, so a file's
//! `use super::…` block is its real dependency list. A reader takes the
//! index's time.

use crate::warning;
use moho::openusd;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4 as GMat4, Vec3};
use tracing::{debug, info};

use crate::aov::AovSource;
use crate::camera::Camera;
use crate::light::LightList;
use crate::live::{LightOrigin, Provenance, StageFacts};
use crate::rt_world::WorldBuilder;
use crate::scene::AssetLoader;
use crate::scene::Scene;
use crate::stats::{MemorySample, RenderStats, SceneCounters, SubdivisionCounters};
use crate::tracer::RenderSettings;
use crate::volume::VolumeRegion;

use openusd::sdf;

pub(in crate::scene) mod adaptive;
pub(in crate::scene) mod assets;
pub(in crate::scene) mod attrs;
pub(in crate::scene) mod camera;
pub(in crate::scene) mod commit;
pub(in crate::scene) mod delegate;
pub(in crate::scene) mod factory;
pub(crate) mod implicit;
pub(in crate::scene) mod index;
pub(in crate::scene) mod instancing;
pub(in crate::scene) mod light_links;
pub(in crate::scene) mod lights;
pub(in crate::scene) mod materials;
pub(in crate::scene) mod mesh;
pub(in crate::scene) mod mtlx_network;
pub(in crate::scene) mod passes;
pub(in crate::scene) mod precomp;
pub(in crate::scene) mod preview;
pub(in crate::scene) mod prims;
pub(in crate::scene) mod products;
pub(in crate::scene) mod settings;
pub(in crate::scene) mod shapes;
pub(in crate::scene) mod strands;
pub(in crate::scene) mod update;
pub(in crate::scene) mod volume;
pub(in crate::scene) mod xform;

use adaptive::{Frustum, ScreenProjection, ScreenRate};
use attrs::{resolve_adaptive_max_level, resolve_subdiv_edge_length, resolve_subdiv_level};
use camera::{RenderCamera, Shutter, ShutterOffsets, screen_projection, shutter_offsets};
use implicit::ImplicitPolicy;
use instancing::ProtoPart;
use light_links::LightLinks;
use lights::{
    DomeInputs, emit_cylinder_light, emit_disk_light, emit_distant_light, emit_dome_light,
    emit_rect_light, emit_sphere_light,
};
use materials::{MaterialCache, mtlx_library};
use mesh::{MeshArena, MeshPlacement, SubdivPolicy, flush_meshes};
use passes::ActivePass;
pub use passes::PassSummary;
use products::import_render_products;
use settings::{
    CameraChoice, CameraPick, check_time_range, dome_light_camera_visibility,
    enable_exposure_compensation, import_render_settings, pick_camera, render_settings_color_space,
    render_settings_dicing_camera, render_settings_subdiv_edge_length,
    render_settings_subdiv_level,
};
use xform::TransformMotion;

/// Everything a commit accumulates into the scene it builds ([`commit`]).
pub(in crate::scene) struct ImportCtx<'a> {
    pub(in crate::scene) world: WorldBuilder,
    pub(in crate::scene) lights: LightList,
    pub(in crate::scene) volumes: Vec<VolumeRegion>,
    /// The camera built to render through, and its prim.
    pub(in crate::scene) camera: Option<(RenderCamera, sdf::Path)>,
    /// The camera to render through, when one was named; see
    /// [`CameraChoice`]. `None` takes the first camera met.
    pub(in crate::scene) wanted_camera: Option<CameraChoice>,
    /// The first camera met, kept while a named one is still being looked
    /// for: it is the fallback when a `RenderSettings.camera` target turns
    /// out not to exist.
    pub(in crate::scene) first_camera: Option<(RenderCamera, sdf::Path)>,
    /// The first camera of a depth-first walk of the index ([`first_camera`]):
    /// the fallback, whatever order the commit reads the prims in.
    pub(in crate::scene) first_camera_path: Option<sdf::Path>,
    /// Every camera prim met, for the error that names the alternatives when
    /// a requested camera is missing. A path per camera, and a stage has a
    /// handful of them.
    pub(in crate::scene) cameras_seen: Vec<sdf::Path>,
    pub(in crate::scene) caches: ImportCaches<'a>,
    /// Direct mesh placements whose representation is not yet decided, in
    /// commit order. Drained by [`flush_meshes`] after the last prim — the
    /// decision needs the whole stage's placement counts, so it cannot be
    /// made prim by prim. Holds ~88 bytes per mesh prim, not per triangle.
    pub(in crate::scene) pending_meshes: Vec<MeshPlacement>,
    /// The `geom_id`s of the static analytic spheres the traversal placed.
    pub(in crate::scene) spheres: Vec<u32>,
    /// Receivers seen and lights whose `collection:lightLink` restricts
    /// them, resolved after the last prim (see [`light_links`]).
    pub(in crate::scene) links: LightLinks,
    /// The active render pass, decided at dispatch (see [`passes`]).
    pub(in crate::scene) pass: Option<ActivePass>,
    /// Each light met, by prim path: where it went in the light list and the
    /// geometry it emitted ([`crate::live::Provenance`]).
    pub(in crate::scene) light_origins: Vec<(String, LightOrigin)>,
    /// The lights the walk met, with their world transforms, in path order:
    /// emitted after it, as the light layer ([`commit::emit_light_layer`]).
    pub(in crate::scene) light_prims: Vec<(sdf::Path, GMat4)>,
    /// The light layer the walk ended with.
    pub(in crate::scene) layer: Option<commit::Layer>,
    pub(in crate::scene) settings: RenderSettings,
    /// How the cameras' apertures are conformed to the image.
    pub(in crate::scene) framing: products::Framing,
    /// The stage file, for resolving asset paths against its directory.
    pub(in crate::scene) stage_path: &'a Path,
    pub(in crate::scene) assets: &'a dyn AssetLoader,
}

/// Emits the light `prim` (its data in the scene index) if it is a UsdLux
/// light, by its USD type as the schema registry types it (`IsA`) — the one dispatch the traversal and an in-place
/// light rebuild ([`update`]) share. `false` when it is not one.
pub(in crate::scene) fn emit_light(
    ctx: &mut ImportCtx,
    prim: &index::data::PrimData,
    world_xf: GMat4,
) -> bool {
    if prim.is_a("SphereLight") {
        emit_sphere_light(ctx, prim, world_xf);
    } else if prim.is_a("RectLight") {
        emit_rect_light(ctx, prim, world_xf);
    } else if prim.is_a("DiskLight") {
        emit_disk_light(ctx, prim, world_xf);
    } else if prim.is_a("CylinderLight") {
        emit_cylinder_light(ctx, prim, world_xf);
    } else if prim.is_a("DistantLight") {
        emit_distant_light(&mut ctx.lights, prim, world_xf, ctx.caches.working);
    } else if let Some(dome) = DomeInputs::of(prim) {
        emit_dome_light(ctx, prim, &dome, world_xf);
    } else {
        return false;
    }
    true
}

/// Why the commit (and `crust ls`) leaves a prim out, from Hydra's flattened
/// values in the index ([`commit::prune`]). An inactive or abstract prim is
/// not in the index at all, as USD removes it from the composed scene.
///
/// - **non-render purpose** — a render draws `default` and `render` purpose
///   only (UsdGeomImageable); Hydra's flattened purpose is
///   `ComputePurpose`. Without it a production asset renders twice: ALab
///   publishes every asset with a `GEO_PROXY` scope (`purpose = "proxy"`)
///   next to its `GEO`, and the proxies drew as grey duplicates of 1 505
///   meshes.
/// - **invisible** — Hydra's flattened visibility is `ComputeVisibility`. It
///   applies to lights as to geometry: ALab's rig parks three interior
///   fills/bounces and a debug dome this way, and all four used to light the
///   shot. The commit still reads an invisible subtree for cameras (see
///   [`commit::commit_prim`]).
///
/// Checked in that order, so the reason reported is the first that applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::scene) enum Prune {
    /// A purpose a final render does not draw (`"proxy"` / `"guide"`).
    Purpose(&'static str),
    /// `visibility = "invisible"`.
    Invisible,
}

impl std::fmt::Display for Prune {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Prune::Purpose(p) => write!(f, "{p}-purpose"),
            Prune::Invisible => f.write_str("invisible"),
        }
    }
}

/// The host's [`UsdImportOptions`](crate::UsdImportOptions) checked before
/// the stage is opened: a bad frame, camera path or working space is refused
/// here, not discovered after a four-minute traversal.
pub(in crate::scene) struct ValidOptions {
    time: Option<f64>,
    camera: Option<sdf::Path>,
    working: Option<crate::color::Space>,
    /// The active render pass ([`passes`]), checked to be a prim path; that
    /// it is a `RenderPass` is checked on the stage, by [`read_index`].
    pass: Option<sdf::Path>,
}

impl ValidOptions {
    pub(in crate::scene) fn check(options: &crate::UsdImportOptions) -> Result<Self, crate::Error> {
        let time = options.frame;
        // The authoritative check: every host reaches the importer through here,
        // and nothing past this point expects a non-finite time.
        if let Some(t) = time
            && !t.is_finite()
        {
            return Err(crate::Error::InvalidFrame(t));
        }
        // Same for the camera: a malformed path is refused before the stage is
        // opened, not discovered after a four-minute traversal.
        let requested_camera = match &options.camera {
            Some(c) => Some(
                sdf::path(c)
                    .ok()
                    .filter(|p| p.is_abs() && p.is_prim() && !p.is_abs_root())
                    .ok_or_else(|| crate::Error::InvalidCameraPath(c.clone()))?,
            ),
            None => None,
        };
        // A working space the host names is refused here, before the stage is
        // opened, like a bad camera path.
        let host_working = match &options.working_space {
            Some(name) => Some(crate::color::working_space(name)?),
            None => None,
        };
        let pass = match &options.pass {
            Some(p) => Some(
                sdf::path(p)
                    .ok()
                    .filter(|p| p.is_abs() && p.is_prim() && !p.is_abs_root())
                    .ok_or_else(|| crate::Error::PassNotFound {
                        path: p.clone(),
                        why: "not an absolute prim path such as /Render/Passes/char".into(),
                    })?,
            ),
            None => None,
        };
        Ok(ValidOptions {
            time,
            camera: requested_camera,
            working: host_working,
            pass,
        })
    }
}

/// The camera the traversal settled on: the one asked for, else — when the
/// stage's `RenderSettings.camera` names one that is not there — the first
/// camera met, else the procedural fallback's. A camera the host asked for by
/// path and the stage does not have is an error naming the alternatives.
/// Returned with the prim it was built from, `None` for the procedural one,
/// whose exposure scale is 1.
fn resolve_camera(ctx: &mut ImportCtx) -> Result<(RenderCamera, Option<String>), crate::Error> {
    // `camera` is the wanted one when it was met, else (nothing named) the
    // first; `first_camera` is the first, kept only while a named one was
    // still being looked for.
    let (built, wanted, first) = (
        ctx.camera.take(),
        ctx.wanted_camera.take(),
        ctx.first_camera.take(),
    );
    let wanted_met = wanted.is_some() && built.is_some();
    let any_met = built.is_some() || first.is_some();
    let with_path = |(c, path): (RenderCamera, sdf::Path)| (c, Some(path.to_string()));
    Ok(match pick_camera(wanted.as_ref(), wanted_met, any_met) {
        CameraPick::Missing(p) => {
            return Err(crate::Error::CameraNotFound {
                path: p.to_string(),
                available: ctx.cameras_seen.iter().map(ToString::to_string).collect(),
            });
        }
        CameraPick::Wanted => with_path(built.expect("the wanted camera was met")),
        CameraPick::First => match (built, first) {
            (Some(c), _) => with_path(c),
            (None, Some((c, first))) => {
                if let Some(p) = wanted.as_ref().map(CameraChoice::path) {
                    warning!(
                        CameraNotACamera,
                        at = p,
                        "RenderSettings.camera targets {p}, which is not a camera on this \
                         stage — rendering through {first} instead"
                    );
                }
                with_path((c, first))
            }
            (None, None) => unreachable!("a camera was met"),
        },
        CameraPick::Procedural => {
            warning!(
                CameraMissing,
                "USD stage has no UsdGeomCamera — falling back to world::get_settings camera"
            );
            // The procedural camera has no aperture to conform: it frames
            // `W / H` of square pixels, and its EXR says so.
            let pixel_aspect = ctx.framing.pixel_aspect;
            if pixel_aspect != 1.0 {
                warning!(
                    CameraProceduralPixelAspect,
                    "pixelAspectRatio = {pixel_aspect} is not honoured by the procedural \
                     camera of a stage with no UsdGeomCamera; it renders square pixels"
                );
            }
            let camera = crate::world::get_settings().0;
            let camera = if ctx.settings.depth_of_field() {
                camera
            } else {
                camera.pinhole()
            };
            (
                RenderCamera {
                    camera,
                    exposure_scale: 1.0,
                    refused_meters_per_unit: None,
                    shutter: ShutterOffsets::default(),
                    lens: None,
                    clipping: Default::default(),
                },
                None,
            )
        }
    })
}

/// The subdivision and displacement counters the traversal accumulated, into
/// `--stats`. `dicer` is the render camera when it is the camera adaptive
/// subdivision read, `None` when a `crust:dicingCamera` dices.
fn record_geometry_counters(meshes: &MeshArena, dicer: Option<&Camera>, stats: &mut RenderStats) {
    if meshes.subdiv.adaptive.is_some() {
        let [interior, stitched] = meshes.subdiv.quality;
        debug!(
            "Per-face triangle shapes (4√3·area / Σ edge², bins ≥0.9 · ≥0.5 · ≥0.1 · ≥0.01 · \
             <0.01): interior {interior:?}, stitched {stitched:?}"
        );
    }
    if let Some(rate) = meshes.subdiv.adaptive {
        // Both reads derive from one `CameraFrame`, but adaptive subdivision
        // may read it off a different stage (the unloaded index while
        // streaming); the two must still agree, or every level was chosen for
        // a viewpoint the render does not use.
        if let Some(camera) = dicer {
            debug_assert!(
                (Vec3::from(camera.origin()) - rate.eye).length()
                    <= 1e-4 * rate.eye.length().max(1.0),
                "adaptive subdivision read the camera at {:?}, the render camera is at {:?}",
                rate.eye,
                camera.origin()
            );
        }
        stats.subdivision = SubdivisionCounters {
            adaptive: Some((rate.target, rate.max)),
            levels: meshes.subdiv.levels.clone(),
            shared_meshes: meshes.subdiv.shared_meshes,
            shared_level: meshes.subdiv.level,
            per_face_meshes: meshes.subdiv.per_face_meshes,
            per_face_fallbacks: meshes.subdiv.per_face_fallbacks,
            rate_bins: meshes.subdiv.rate_bins.clone(),
        };
    }

    stats.displacement = crate::stats::DisplacementCounters {
        frustum_skipped: meshes.subdiv.frustum_skipped,
        ..meshes.displaced.clone()
    };
}

/// What the host hands a commit besides the index: what only it knows, the
/// stage's file, and the import's clock and warnings (design D1).
pub(in crate::scene) struct Commit<'a> {
    pub(in crate::scene) facts: &'a dyn index::host::HostFacts,
    /// The stage's file: the anchor of an asset path openusd handed back
    /// unresolved.
    pub(in crate::scene) path: &'a Path,
    /// When the host started the import and opened the stage, and how long
    /// its stage checks took.
    pub(in crate::scene) import_start: Instant,
    pub(in crate::scene) open_start: Instant,
    pub(in crate::scene) checks_elapsed: Option<Duration>,
    /// Every coded warning of the commit, the host's included.
    pub(in crate::scene) warning_scope: crate::WarningScope,
    /// The environment maps a kept delegate shares across its commits.
    pub(in crate::scene) environments: Option<&'a lights::Environments>,
}

/// The delegate a commit reads: the batch host's, consumed (dropped with the
/// host's stage before the top-level commit, the import's memory peak), or a
/// kept one a host updates again.
pub(in crate::scene) enum Holder<'a> {
    Batch {
        delegate: delegate::HydraScene,
        /// Releases the host's stage once the delegate is dropped.
        release: Box<dyn FnOnce() + 'a>,
    },
    Kept(&'a delegate::HydraScene),
}

/// Commits the delegate's managed prims into a [`Scene`] (design D3).
pub(in crate::scene) fn commit_scene(
    holder: Holder<'_>,
    commit: Commit<'_>,
    assets: &dyn AssetLoader,
    options: &crate::UsdImportOptions,
    valid: ValidOptions,
) -> Result<Scene, crate::Error> {
    let Commit {
        facts,
        path,
        import_start,
        open_start,
        checks_elapsed,
        warning_scope,
        environments,
    } = commit;
    let time_range = facts.time_range();
    let time = valid.time;
    let mut stats = RenderStats::new();
    let delegate: &delegate::HydraScene = match &holder {
        Holder::Batch { delegate, .. } => delegate,
        Holder::Kept(delegate) => delegate,
    };
    let top = delegate.index();
    if let Some(t) = time {
        check_time_range(time_range, t);
    }
    let IndexRead {
        settings,
        products,
        domes_seen_by_camera,
        exposure_compensation,
        working,
        wanted_camera,
        subdiv,
        projection,
        dicing,
        shutter,
    } = read_index(facts, top.as_ref(), path, options, &valid)?;
    let pass = valid.pass.as_ref();
    let render_settings = settings::render_settings_path(top.as_ref(), pass).map(|p| p.to_string());
    let settings_prim = settings::read_settings_prim(top.as_ref(), pass).map(|p| p.to_string());
    // The stage checks are their own phase, not the open's.
    let open_elapsed = open_start
        .elapsed()
        .saturating_sub(checks_elapsed.unwrap_or_default());
    let open_mem = MemorySample::now();
    let asks_time = asks_about_time(&shutter, &products);
    let frame_posed = frame_pose(&shutter, &products);

    let mut ctx = ImportCtx {
        world: {
            let mut world = WorldBuilder::new();
            if options.keep_geometry {
                world.keep_inputs();
            }
            world
        },
        lights: {
            let mut lights = LightList::new();
            lights.set_luma(crate::color::luma(working));
            lights
        },
        volumes: Vec::new(),
        camera: None,
        wanted_camera,
        first_camera: None,
        first_camera_path: None,
        cameras_seen: Vec::new(),
        // Prims binding the same material path share one Arc, and prims
        // with identical local geometry + material share one copy of that
        // geometry — placed by an instance when it is placed more than once,
        // baked flat into the parent BVH when it is placed exactly once.
        caches: ImportCaches {
            implicit: ImplicitPolicy::new(projection),
            motion: TransformMotion::new(
                shutter.interval,
                shutter.offsets.open == 0.0,
                frame_posed,
                asks_time,
            )
            .with_frame(time),
            time,
            environments,
            ..ImportCaches::new(assets, facts, path, subdiv, working, mtlx_library())
        },
        pending_meshes: Vec::new(),
        spheres: Vec::new(),
        links: LightLinks::default(),
        pass: pass.map(|p| ActivePass::read(top.as_ref(), p)),
        light_origins: Vec::new(),
        light_prims: Vec::new(),
        layer: None,
        settings,
        framing: products.framing,
        stage_path: path,
        assets,
    };
    // The primvars the render vars read, resolved on the index stage before
    // any geometry: every mesh read from here keeps exactly these.
    ctx.caches.meshes.primvars = products.primvars.clone();

    let traverse_start = Instant::now();
    let paths = delegate.paths();
    if ctx.caches.meshes.subdiv.adaptive.is_some() {
        commit::count_placements(top.as_ref(), &paths, &mut ctx.caches);
    }
    ctx.first_camera_path = first_camera(top.as_ref());
    commit::visit_all(&top, &paths, &mut ctx);
    // Released before the top-level BVH commit, the import's memory peak.
    if let Holder::Batch { delegate, release } = holder {
        if options.skip_stage_teardown {
            // The index holds the stage too: left allocated with it.
            std::mem::forget(top);
            std::mem::forget(delegate);
        } else {
            drop(top);
            drop(delegate);
        }
        release();
    } else {
        drop(top);
    }

    // The traverse also builds each mesh's and prototype's kernel scene,
    // so its own BVH work is inside this figure; the separate "Commit
    // acceleration structure" phase below is the *top-level* build.
    // Host asset decoding (environment maps, Ptex) happens *during* traversal but
    // is reported as its own phase, so it comes back out of this figure.
    let asset_time = ctx.caches.asset_time;
    let traverse_elapsed = traverse_start.elapsed().saturating_sub(asset_time);
    ctx.caches.unread.report();
    report_unmoved(
        &ctx.caches.motion,
        &shutter,
        products.request.asks_for(AovSource::MotionVector),
    );
    let traverse_mem = MemorySample::now();
    debug!(
        "Traversal done in {:?}, plus {:?} of host asset decoding taken out of it and \
         reported as its own phase: {} geometries, {} light(s), {} volume region(s)",
        traverse_elapsed,
        asset_time,
        ctx.world.count(),
        ctx.lights.count(),
        ctx.volumes.len()
    );

    let (render_camera, camera_path) = resolve_camera(&mut ctx)?;
    render_camera.warn_lens_units(camera_path.as_deref());
    render_camera.warn_clipping(camera_path.as_deref());
    check_shutter_camera(&shutter, camera_path.as_deref(), render_camera.shutter);
    let RenderCamera {
        camera,
        exposure_scale,
        lens,
        ..
    } = render_camera;
    let settings = apply_exposure(
        settings,
        exposure_compensation,
        exposure_scale,
        camera_path.as_deref(),
    );

    // Every receiver and every light has been seen, so a light's link can be
    // judged against all of them; before `commit`, which freezes geometry
    // masks.
    let layer = ctx.layer.take().unwrap_or_else(|| commit::Layer {
        links: LightLinks::default(),
        paths: Vec::new(),
    });
    let receivers = std::mem::take(&mut ctx.links).record;
    let mut geometry_runs = receivers.geometry_runs();
    geometry_runs.extend(layer.links.record.geometry_runs());
    let volume_paths = receivers.volume_paths();
    let volume_masks: Vec<_> = ctx.volumes.iter().map(|v| v.mask).collect();
    let world = &ctx.world;
    let tables = light_links::judge(
        &receivers,
        &layer.links,
        &ctx.lights,
        world.count(),
        &|id| world.mask(id),
        &volume_masks,
    );
    let remap = light_links::install(tables, &mut ctx.world, &mut ctx.lights, &mut ctx.volumes);
    // The provenance names each light by its place in the list as the links
    // left it; a light they took out of the list is no longer one to rebuild,
    // though the layer still holds it (`light_paths`).
    ctx.light_origins
        .retain_mut(|(_, origin)| match remap[origin.index] {
            Some(index) => {
                origin.index = index;
                true
            }
            None => false,
        });
    hide_lights_at_infinity(domes_seen_by_camera, &mut ctx.lights);

    let dicer = dicing.is_none().then_some(&camera);
    record_geometry_counters(&ctx.caches.meshes, dicer, &mut stats);
    let implicit = &ctx.caches.implicit;
    if implicit.shapes > 0 {
        debug!(
            "Implicit shapes: {} tessellated into {} triangles; curved ones by radial count \
             {:?}{}",
            implicit.shapes,
            implicit.triangles,
            implicit.counts,
            if implicit.projection.is_some() {
                ""
            } else {
                " (no camera named before the traversal: the fixed count)"
            }
        );
    }

    let pending = std::mem::take(&mut ctx.pending_meshes);
    let (mesh_origins, mesh_slots) = flush_meshes(&mut ctx.world, &mut ctx.caches.meshes, pending);

    let commit_start = Instant::now();
    let committed = ctx.world.commit();
    let commit_elapsed = commit_start.elapsed();
    debug!("Top-level acceleration structure committed in {commit_elapsed:?}");
    let commit_mem = MemorySample::now();

    // Memory is sampled where each phase actually ended, not here — the
    // phases are all recorded together, so `record`'s sample-now would
    // give every one of them the same figures.
    stats.record_at("Parse USD stage", 0, import_start.elapsed(), commit_mem);
    stats.record_at("Open stage", 1, open_elapsed, open_mem);
    if let Some(elapsed) = checks_elapsed {
        stats.record_at("Stage checks", 1, elapsed, open_mem);
    }
    stats.record_at("Traverse prims", 1, traverse_elapsed, traverse_mem);
    if !asset_time.is_zero() {
        stats.record_at("Load assets", 1, asset_time, traverse_mem);
    }
    stats.record_at(
        "Commit acceleration structure",
        1,
        commit_elapsed,
        commit_mem,
    );

    stats.scene = SceneCounters {
        geometries: committed.count(),
        top_level: committed.primitive_breakdown().into(),
        unique: committed.unique_primitive_breakdown().into(),
        footprint: committed.memory_footprint(),
        strand_tables_bytes: committed.strand_table_bytes(),
        primvar_tables_bytes: committed.primvar_table_bytes(),
        lights: ctx.lights.count(),
        volumes: ctx.volumes.len(),
    };
    stats.image = (&settings).into();

    let mut request = products.request;
    if let Some(pc) = request.precomp.as_mut() {
        if let Some(pc) = std::sync::Arc::get_mut(pc) {
            pc.load_kernels(|p| assets.load_light_texture(p, crate::ColorSpace::RAW));
        }
        // Nothing lights a stage with no light, so its beauty is black and the
        // stop rule, which reads only the beauty, sees no variance.
        if ctx.lights.count() == 0 && pc.has_filters() {
            warning!(
                PrecompBlackBeauty,
                "The stage has a precomp but no light, so its beauty is black: adaptive \
                 sampling stops every pixel at crust:minSamplesPerPixel. Author \
                 crust:minSamplesPerPixel = crust:samplesPerPixel for the precomp to take every \
                 sample"
            );
        }
    }
    let mut scene = Scene::new(camera, committed, ctx.lights, settings).with_volumes(ctx.volumes);
    scene.stats = stats;
    scene.aovs = request;
    scene.working_space = working;
    scene.camera_path = camera_path;
    scene.time = time;
    scene.warnings = warning_scope.finish();
    scene.layers = facts.layers();
    scene.pass = ctx.pass.as_ref().map(ActivePass::summary);
    scene.settings_prim = settings_prim;
    scene.source = crate::scene::Source {
        lens,
        depth_of_field: scene.camera.has_depth_of_field(),
        motion_blur: scene.settings.motion_blur() && shutter.interval.is_some(),
        layer_stack: facts.layer_stack(),
        invisible_overrides: facts.invisible_overrides(),
    };
    scene.provenance = Some(Provenance {
        materials: ctx.caches.materials.origins(&scene.world),
        material_reads: ctx.caches.materials.reads(),
        lights: ctx.light_origins.into_iter().collect(),
        geometry: geometry_runs,
        meshes: mesh_origins,
        mesh_slots,
        spheres: std::mem::take(&mut ctx.spheres),
        volumes: volume_paths,
        light_paths: layer.paths,
        prototypes: std::mem::take(&mut ctx.caches.prototypes),
        links: receivers,
        volume_masks,
        stage: StageFacts {
            options: options.clone(),
            render_settings,
            subdiv_level: ctx.caches.meshes.subdiv.level,
            adaptive: ctx.caches.meshes.subdiv.adaptive.map(|r| r.key()),
            domes_seen_by_camera,
            precomp: scene.aovs.precomp.is_some(),
            working: Some(working),
            camera: scene.camera_path.clone(),
            cameras: ctx.cameras_seen.iter().map(ToString::to_string).collect(),
            dicing: dicing.map(|p| p.to_string()),
            on_screen_shapes: ctx
                .caches
                .implicit
                .on_screen
                .then(|| ctx.caches.implicit.projection.map(|p| p.key()))
                .flatten(),
            unsized_shapes: ctx.caches.implicit.fixed_unshared,
            primvars: ctx.caches.meshes.primvars.key(),
            shutter: shutter.key(asks_time, frame_posed),
            mtlx_library: ctx.caches.materials.library.clone(),
        },
    });
    Ok(scene)
}

/// Hides every light at infinity from the camera when the settings'
/// `domeLightCameraVisibility` is off: after the links, at the commit and
/// when the light layer is built again.
pub(in crate::scene) fn hide_lights_at_infinity(
    domes_seen_by_camera: bool,
    lights: &mut LightList,
) {
    if !domes_seen_by_camera {
        debug!(
            "domeLightCameraVisibility = false: every light at infinity is hidden from the camera"
        );
        lights.hide_infinite_from_camera();
    }
}

/// What the import reads before it traverses anything: the index stage's
/// render settings and products, and the stage-wide choices made from them.
/// The settings carry the products' resolution, region and motion blur, and
/// the frame's seed; the camera's exposure comes later, from the camera.
pub(in crate::scene) struct IndexRead {
    pub(in crate::scene) settings: RenderSettings,
    pub(in crate::scene) products: products::RenderProducts,
    pub(in crate::scene) domes_seen_by_camera: bool,
    pub(in crate::scene) exposure_compensation: bool,
    pub(in crate::scene) working: crate::color::Space,
    pub(in crate::scene) wanted_camera: Option<CameraChoice>,
    pub(in crate::scene) subdiv: SubdivPolicy,
    /// The projection tessellation reads — the dicing camera's — when a
    /// camera was named (see [`subdiv_policy`]).
    pub(in crate::scene) projection: Option<ScreenProjection>,
    /// `crust:dicingCamera`'s target, when it is a camera ([`Dicer`]).
    pub(in crate::scene) dicing: Option<sdf::Path>,
    /// The interval motion is measured over ([`read_camera_ahead`]).
    pub(in crate::scene) shutter: Shutter,
}

/// Reads [`IndexRead`] off the payload-free `index` stage: once by
/// [`commit_scene`], and again by an in-place settings update ([`update`]), so
/// the two read the same settings by construction.
pub(in crate::scene) fn read_index(
    host: &dyn index::host::HostFacts,
    scene: &dyn moho::scene_index::SceneIndex,
    path: &Path,
    options: &crate::UsdImportOptions,
    valid: &ValidOptions,
) -> Result<IndexRead, crate::Error> {
    // The active pass first: it may choose the settings prim every read
    // below goes through.
    let pass = valid.pass.as_ref();
    if let Some(p) = pass {
        settings::check_pass(scene, host, p)?;
        debug!("Rendering pass {p}");
    }
    // Render settings come first — the camera importer needs the aspect ratio.
    let mut settings = import_render_settings(scene, pass);
    // The products the render writes. The first one's camera and resolution
    // are the render's: the settings' own unless that product overrides them.
    let products = import_render_products(scene, Some(path), pass);
    if let Some((w, h)) = products.resolution {
        settings = settings.with_resolution(w, h);
    }
    // The region, after the resolution it is counted in. A host's region
    // (`--region`) replaces it later, on the settings.
    if let Some(window) = products.data_window {
        let (w, h) = settings.get_dimensions();
        if let Some(region) = products::region_from_ndc(window, w, h) {
            settings = settings
                .with_region(region)
                .expect("region_from_ndc returns a region inside the frame");
            debug!("dataWindowNDC: rendering region {region} of the {w}x{h} frame");
        }
    }
    settings = settings
        .with_motion_blur(products.motion_blur)
        .with_depth_of_field(products.depth_of_field);
    let domes_seen_by_camera = dome_light_camera_visibility(scene, pass);
    let exposure_compensation = enable_exposure_compensation(scene, pass);
    // The working colour space, before any colour is read: every texture
    // request and authored colour is converted into it.
    let working = valid
        .working
        .unwrap_or_else(|| render_settings_color_space(scene, pass));
    debug!("Rendering in {} ({})", working.label(), working.name());
    // Geometry, not tracer, settings: every mesh whose scheme is not `none`
    // is refined to this one level, so it must be known before the traversal.
    let subdiv_level = resolve_subdiv_level(
        options.subdivision_level,
        render_settings_subdiv_level(scene, pass),
    );
    debug!("Subdivision level {subdiv_level} for every mesh whose subdivisionScheme is not none");
    // Which camera to render through: the host's explicit choice, else the
    // stage's own `RenderSettings.camera`. Decided here, on the index stage,
    // because the traversal needs it before it meets any camera.
    let wanted_camera =
        settings::wanted_camera(valid.camera.clone(), products.camera.clone(), scene, pass);
    if let Some(choice) = &wanted_camera {
        debug!("Rendering through {choice}");
    }
    // The camera tessellation reads: `crust:dicingCamera` when it names one,
    // else the render camera. Products do not override it.
    let (dicer, shutter) = read_camera_ahead(
        scene,
        wanted_camera.as_ref(),
        render_settings_dicing_camera(scene, pass).as_ref(),
        &settings,
        products.framing,
        valid.time,
    )?;
    if let Some((open, close)) = shutter.interval {
        debug!("Motion is measured over time codes {open} .. {close}");
    }
    let dicing = dicer.authored.then(|| dicer.path.clone()).flatten();
    let (subdiv, projection) = subdiv_policy(scene, options, pass, subdiv_level, dicer);
    if let Some(t) = valid.time {
        // The sampler's frame seed follows the frame being rendered, so an
        // image sequence gets independent noise per frame instead of one
        // pattern swimming over moving geometry. Integer part only: the
        // seed is an integer, and a subframe shares its frame's seed.
        let seed = t.floor() as isize;
        debug!("Frame {t} sets the sampler frame seed to {seed} (over crust:frame)");
        settings = settings.with_frame(seed);
    }
    Ok(IndexRead {
        settings,
        products,
        domes_seen_by_camera,
        exposure_compensation,
        working,
        wanted_camera,
        subdiv,
        projection,
        dicing,
        shutter,
    })
}

/// `settings` with the render camera's exposure scale applied:
/// `enableExposureCompensation = false` renders at 1, as Hydra does. The
/// scale is checked here, once, for the camera rendered through: a
/// candidate the traversal built and dropped, or an exposure turned off,
/// warns about nothing.
pub(in crate::scene) fn apply_exposure(
    settings: RenderSettings,
    exposure_compensation: bool,
    exposure_scale: f32,
    camera_path: Option<&str>,
) -> RenderSettings {
    if !exposure_compensation {
        debug!("enableExposureCompensation = false: the camera's exposure is not applied");
        settings
    } else if exposure_scale.is_finite() && exposure_scale > 0.0 {
        settings.with_exposure_scale(exposure_scale)
    } else {
        let camera = camera_path.unwrap_or("the render camera");
        warning!(
            CameraInvalidExposure,
            at = camera,
            "{camera}: the exposure attributes give a scale of {exposure_scale} — the image is \
             not scaled"
        );
        settings
    }
}

/// The camera tessellation reads, as [`read_camera_ahead`] read it: the
/// dicing camera.
pub(in crate::scene) struct Dicer {
    /// `crust:dicingCamera`'s target when it is a camera, else the render
    /// camera's path when one is named; `None` when neither is.
    path: Option<sdf::Path>,
    /// Its screen projection; `None` when `path` is not a camera on the stage.
    projection: Option<camera::ScreenProjection>,
    /// Whether `path` is `crust:dicingCamera`'s target.
    authored: bool,
}

/// The cameras as the import reads them before the traversal: the dicing
/// camera's screen projection ([`Dicer`]), and the render camera's shutter.
///
/// A named camera — the render camera, or the `dicing` target — is read from
/// the index stage when it is composed there, else from a stage
/// population-masked to its path with payloads loaded (a shot camera under a
/// payload is the production case). A `dicing` target that is not a camera
/// warns, and the render camera dices. With no render camera named, or a named
/// path that is not a camera, the shutter comes from the first camera of a
/// depth-first walk of the index ([`first_camera`]), and without a dicing camera
/// there is no projection; the traversal may still settle on another camera,
/// which [`check_shutter_camera`] reports.
fn read_camera_ahead(
    scene: &dyn moho::scene_index::SceneIndex,
    wanted_camera: Option<&CameraChoice>,
    dicing: Option<&sdf::Path>,
    settings: &RenderSettings,
    framing: products::Framing,
    frame: Option<f64>,
) -> Result<(Dicer, Shutter), crate::Error> {
    let read = |camera_path: &sdf::Path| -> Result<_, crate::Error> {
        Ok(shutter_offsets(scene, camera_path)
            .map(|s| (screen_projection(scene, camera_path, settings, framing), s)))
    };
    let named = match wanted_camera {
        Some(choice) => read(choice.path())?.map(|r| (choice.path().clone(), r)),
        None => None,
    };
    // Only a prim path names a camera: a property path would be read as its
    // prim (the schema lookup takes the prim part) and cannot be masked to.
    let diced = match dicing {
        Some(target) => match target
            .is_prim()
            .then(|| read(target))
            .transpose()?
            .flatten()
        {
            Some((projection, _)) => Some(Dicer {
                path: Some(target.clone()),
                projection,
                authored: true,
            }),
            None => {
                warning!(
                    CameraDicingNotCamera,
                    at = target,
                    "crust:dicingCamera targets {target}, which is not a camera on this stage — \
                     tessellation reads the render camera"
                );
                None
            }
        },
        None => None,
    };
    let shutter = match &named {
        Some((camera_path, (_, offsets))) => {
            Shutter::new(Some(camera_path.clone()), *offsets, frame)
        }
        None => {
            let first = first_camera(scene);
            let offsets = first
                .as_ref()
                .and_then(|p| shutter_offsets(scene, p))
                .unwrap_or_default();
            Shutter::new(first, offsets, frame)
        }
    };
    let dicer = diced.unwrap_or_else(|| Dicer {
        path: wanted_camera.map(|c| c.path().clone()),
        projection: named.and_then(|(_, (projection, _))| projection),
        authored: false,
    });
    Ok((dicer, shutter))
}

/// The first camera of a depth-first walk of the index (a stack, so the last
/// child first), with the commit's pruning: an invisible subtree still
/// walked, instances and point instancers not entered.
pub(in crate::scene) fn first_camera(
    scene: &dyn moho::scene_index::SceneIndex,
) -> Option<sdf::Path> {
    use moho::tokens::{prim_type, usd_prim_info};
    let mut stack = vec![(sdf::Path::abs_root(), false)];
    while let Some((path, parent_hidden)) = stack.pop() {
        let prim = index::data::PrimData::at(scene, &path);
        if !path.is_abs_root() && !prim.is_defined() {
            continue;
        }
        let pruned = commit::prune(scene, &path);
        let hidden = parent_hidden || pruned == Some(Prune::Invisible);
        if pruned.is_some_and(|r| r != Prune::Invisible) {
            // A purpose a render does not draw: no camera here, but maybe
            // below.
            stack.extend(
                commit::children(scene, &path)
                    .into_iter()
                    .map(|c| (c, hidden)),
            );
            continue;
        }
        if prim
            .locate([
                usd_prim_info::USD_PRIM_INFO,
                usd_prim_info::NI_PROTOTYPE_PATH,
            ])
            .is_some()
        {
            continue;
        }
        if prim.ty() == prim_type::CAMERA {
            return Some(path);
        }
        if !hidden && prim.ty() == prim_type::INSTANCER {
            continue;
        }
        stack.extend(
            commit::children(scene, &path)
                .into_iter()
                .map(|c| (c, hidden)),
        );
    }
    None
}

/// Whether the import asks prims whether their transform varies over time:
/// when the shutter's offsets open an interval, or a `motionvector` var
/// would report zero for what does ([`TransformMotion`]).
pub(in crate::scene) fn asks_about_time(
    shutter: &Shutter,
    products: &products::RenderProducts,
) -> bool {
    shutter.offsets.opens() || products.request.asks_for(AovSource::MotionVector)
}

/// Whether moving placements are posed at the frame rather than at shutter
/// open: `disableMotionBlur` (not `instantaneousShutter` alone) over a
/// shutter that opens an interval away from the frame. Opening at the frame,
/// the two poses are one.
pub(in crate::scene) fn frame_pose(shutter: &Shutter, products: &products::RenderProducts) -> bool {
    products.sharp_at_frame
        && !products.motion_blur
        && shutter.interval.is_some()
        && shutter.offsets.open != 0.0
}

/// The summarised warnings for time-varying transforms the render does not
/// move ([`TransformMotion`]): over a shutter with no frame to place it at,
/// over an instantaneous shutter while a `motionvector` var is requested, and
/// on prims that get no motion whatever the shutter.
fn report_unmoved(motion: &TransformMotion, shutter: &Shutter, motion_vectors: bool) {
    let n = motion.varying.count;
    let first = motion.varying.first.as_deref().unwrap_or("");
    let prims = if n == 1 { "prim's" } else { "prims'" };
    if n > 0 && shutter.needs_frame() {
        let camera = shutter
            .camera
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        warning!(
            TimeMotionNeedsFrame,
            at = first,
            "{camera} opens its shutter ({} .. {}), but no frame was given: {n} drawn {prims} \
             time-sampled transforms are not evaluated over it and render sharp (the first at \
             {first}). Render a frame with -f",
            shutter.offsets.open,
            shutter.offsets.close
        );
    } else if n > 0 && motion_vectors && !shutter.offsets.opens() {
        warning!(
            CameraShutterInstantaneous,
            at = first,
            "A motionvector var is requested, but the camera's shutter is instantaneous: the \
             time-sampled transforms of {n} drawn {prims} move nothing and their vectors read \
             zero (the first at {first}). Author the camera's shutter:open / shutter:close to \
             measure them"
        );
    }
    let n = motion.ignored.count;
    if n > 0 {
        let first = motion.ignored.first.as_deref().unwrap_or("");
        let one = n == 1;
        warning!(
            XformMotionIgnored,
            at = first,
            "{n} prim{} whose transform moves over the shutter get{} no motion (curves, lights, \
             volumes, prims inside a prototype and baked non-invertible meshes are not moved); \
             {} render{} at shutter open (the first at {first})",
            if one { "" } else { "s" },
            if one { "s" } else { "" },
            if one { "it" } else { "they" },
            if one { "s" } else { "" },
        );
    }
}

/// Warns when the camera the traversal settled on (`rendered`, `None` for the
/// procedural one) is not the one the shutter was read from ahead of it, and
/// the two shutters differ: motion was measured over the other camera's.
fn check_shutter_camera(
    shutter: &Shutter,
    rendered: Option<&str>,
    rendered_offsets: ShutterOffsets,
) {
    let read = shutter.camera.as_ref().map(ToString::to_string);
    if read.as_deref() == rendered || shutter.offsets == rendered_offsets {
        return;
    }
    let name = |c: Option<&str>| c.unwrap_or("the procedural camera").to_owned();
    let (read, rendered) = (name(read.as_deref()), name(rendered));
    let ShutterOffsets { open, close } = shutter.offsets;
    warning!(
        CameraShutterMismatch,
        at = rendered.as_str(),
        "The render goes through {rendered}, but motion was measured over the shutter of \
         {read}, the camera found before the traversal ({open} .. {close}): name the camera \
         with --camera or RenderSettings.camera to measure over its own"
    );
}

/// How this load refines subdivision surfaces — one `subdiv_level` for every
/// mesh, or, when a target edge length is set, adaptively from the dicing
/// camera — and that camera's [`ScreenProjection`], which also sizes implicit
/// shapes ([`implicit::ImplicitPolicy`]).
///
/// `dicer` is the dicing camera as [`read_camera_ahead`] read it. With no
/// camera named, neither a dicing nor a render camera, or when the path is
/// not a camera, there is no projection: adaptive mode then warns once and the
/// uniform level applies, and implicit shapes take their fixed count,
/// silently. Finding the first camera's projection would take a traversal of
/// its own.
fn subdiv_policy(
    scene: &dyn moho::scene_index::SceneIndex,
    options: &crate::UsdImportOptions,
    pass: Option<&sdf::Path>,
    subdiv_level: u32,
    dicer: Dicer,
) -> (SubdivPolicy, Option<ScreenProjection>) {
    let target = resolve_subdiv_edge_length(
        options.subdivision_edge_length,
        render_settings_subdiv_edge_length(scene, pass),
    )
    .filter(|_| crate::config().subdiv);
    let Some(camera_path) = dicer.path else {
        if let Some(target) = target {
            warning!(
                SubdivAdaptiveNeedsCamera,
                "Adaptive subdivision ({target} px) needs the render camera before the stage \
                 is read — name it with --camera or RenderSettings.camera, or a dicing camera \
                 with crust:dicingCamera. Using the uniform level {subdiv_level}"
            );
        }
        return (SubdivPolicy::new(subdiv_level), None);
    };
    let projection = dicer.projection.map(|p| ScreenProjection {
        eye: p.eye,
        f_px: p.f_px,
        orthographic: p.orthographic,
        frustum: crate::config().adaptive_frustum.then(|| {
            if p.orthographic {
                Frustum::orthographic(p.eye, p.forward, p.up, p.tan_v, p.tan_h, p.centre)
            } else {
                Frustum::new(p.eye, p.forward, p.up, p.tan_v, p.tan_h, p.centre)
            }
        }),
    });
    let Some(target) = target else {
        return (SubdivPolicy::new(subdiv_level), projection);
    };
    let Some(projection) = projection else {
        warning!(
            SubdivAdaptiveNeedsCamera,
            at = camera_path,
            "Adaptive subdivision ({target} px): {camera_path} is not a camera on this stage. \
             Using the uniform level {subdiv_level}"
        );
        return (SubdivPolicy::new(subdiv_level), None);
    };
    let max = resolve_adaptive_max_level(
        options.subdivision_level,
        render_settings_subdiv_level(scene, pass),
    );
    info!(
        "Adaptive subdivision: unshared cage edges refined to at most {target} px through \
         {camera_path}, up to level {max}; shared prototypes at level {subdiv_level}"
    );
    // Shared prototypes take the uniform level: the level setting when one is
    // given (it is also the ceiling above), else 0.
    (
        SubdivPolicy::adaptive(
            ScreenRate {
                projection,
                target,
                max,
            },
            subdiv_level,
        ),
        Some(projection),
    )
}

// -----------------------------------------------------------------------
// Import-wide state and traversal predicates
// -----------------------------------------------------------------------

/// The importer's memoization, threaded through geometry import.
///
/// All three caches exist for the same reason — authored geometry should
/// be turned into kernel geometry exactly once, however many prims,
/// instances or prototypes refer to it.
pub(in crate::scene) struct ImportCaches<'a> {
    /// Material path (memoized) → shared material.
    pub(in crate::scene) materials: MaterialCache,
    /// Distinct meshes by content + material. Holds each one's triangles
    /// until its representation is decided (see [`flush_meshes`]), then its
    /// committed kernel scene if it needed one.
    pub(in crate::scene) meshes: MeshArena,
    /// A prototype's root in the instancing chain → its shared parts, for both
    /// instancing mechanisms: Moho's propagated prototype path (or a point
    /// instancer's copy), so a prototype is built once for the whole import.
    /// A prototype placed once in adaptive mode is built
    /// for its placement and not cached here.
    pub(in crate::scene) protos: HashMap<String, Arc<Vec<ProtoPart>>>,
    /// The same prototypes as one [`ProtoPart`] each (see
    /// `instancing::group_parts`), `None` for one with no geometry. Same keys
    /// as `protos`.
    pub(in crate::scene) groups: HashMap<String, Option<ProtoPart>>,
    /// Each prototype placed at top level, by the key of `protos`: its
    /// placed parts' layout and placements, for the provenance.
    pub(in crate::scene) prototypes: HashMap<String, crate::live::ProtoFacts>,
    /// Adaptive mode only: native placements per prototype path over the
    /// whole stage, counted before the commit ([`commit::count_placements`]). A
    /// prototype placed once is unshared, and refined by its size on screen.
    pub(in crate::scene) placements: HashMap<String, u32>,
    /// The host's decoder, for materials that carry a texture asset.
    pub(in crate::scene) assets: &'a dyn AssetLoader,
    /// The index's time code (`sceneGlobals/currentFrame`), for the
    /// host's transform diagnostics.
    pub(in crate::scene) time: Option<f64>,
    /// The delegate's terminal index ([`index::Input::chain`]), set for the
    /// commit's duration.
    pub(in crate::scene) scene: Option<moho::scene_index::SceneIndexHandle>,
    /// What the host knows of its stage beyond scene description.
    pub(in crate::scene) host: &'a dyn index::host::HostFacts,
    /// The top of the delegate's chain while a commit runs: the material
    /// readers read networks there.
    pub(in crate::scene) top: Option<moho::scene_index::SceneIndexHandle>,
    /// The index the instancers are read from while a commit runs
    /// ([`index::Input::chain`]): Moho's instancers and their prototypes.
    pub(in crate::scene) inst: Option<moho::scene_index::SceneIndexHandle>,
    /// Root layer, the fallback anchor for an asset path openusd handed back
    /// unresolved.
    pub(in crate::scene) stage_path: &'a Path,
    /// Time the host spent decoding assets — environment maps *and* Ptex files.
    ///
    /// One accumulator for both, deliberately: it is reported as the "Load
    /// assets" phase and subtracted out of the traversal figure, so a second
    /// one that nobody folded in would silently bill texture loading to
    /// traversal. That is exactly what a separate `ptex_time` did, and on a
    /// Ptex-heavy stage the host's decode can dominate the import.
    pub(in crate::scene) asset_time: Duration,
    /// Resolved `.ies` path → the decoded profile (`None`: the host declined).
    /// A light rig commonly points dozens of fixtures at one profile.
    pub(in crate::scene) ies: HashMap<std::path::PathBuf, Option<Arc<crate::IesProfile>>>,
    /// Resolved path → a `RectLight`'s decoded colour map, for the same reason.
    pub(in crate::scene) light_textures:
        HashMap<(std::path::PathBuf, crate::ColorSpace), Option<Arc<crate::LightTexture>>>,
    /// A kept delegate's environment maps, shared across its commits and
    /// rebuilds ([`lights::Environments`]); `None` for a one-off commit.
    pub(in crate::scene) environments: Option<&'a lights::Environments>,
    /// Asset files the host failed to read, as opposed to ones it declined on
    /// purpose ([`assets::explained`]): only references to these count a
    /// `*.unreadable` warning.
    pub(in crate::scene) failed_assets: std::collections::HashSet<std::path::PathBuf>,
    /// How implicit shapes are tessellated, and what was.
    pub(in crate::scene) implicit: ImplicitPolicy,
    /// Geometry prims no reader reads, for `geom.unsupported_type`.
    pub(in crate::scene) unread: UnreadGprims,
    /// How time-sampled transforms move over the shutter, and what did not.
    pub(in crate::scene) motion: TransformMotion,
    /// The working colour space every texture and authored colour is
    /// converted into ([`crate::color`]).
    pub(in crate::scene) working: crate::color::Space,
    /// Its luminance weights, which every material's lobe selection weighs
    /// colours by ([`crate::color::luma`]).
    pub(in crate::scene) luma: utils::Luma,
}

/// Geometry prims that no reader reads as a surface or a volume — a
/// `Points`, a `NurbsPatch`, a future type — by type, in first-seen order: one
/// per prim the commit meets, one per prim per prototype build.
/// Reported once per type at the end of the load ([`UnreadGprims::report`]).
#[derive(Default)]
pub(in crate::scene) struct UnreadGprims {
    /// `(type name, first prim, prims read)`.
    types: Vec<(String, String, u64)>,
}

impl UnreadGprims {
    /// Counts `prim`, which every reading branch has passed over, when it is
    /// a `Gprim`: a camera, a light, a material or a transform is not one.
    fn saw(&mut self, prim: &index::data::PrimData) {
        if !prim.is_a("Gprim") {
            return;
        }
        let ty = prim.usd_type();
        match self.types.iter_mut().find(|(t, ..)| *t == ty) {
            Some((.., n)) => *n += 1,
            None => self.types.push((ty, prim.path().to_string(), 1)),
        }
    }

    /// One `geom.unsupported_type` per type, carrying its count — raised at
    /// the end of the load, so a type met many times is still one
    /// warning.
    fn report(&self) {
        for (ty, first, n) in &self.types {
            let prims = if *n == 1 { "prim" } else { "prims" };
            warning!(
                GeomUnsupportedType,
                at = first.as_str(),
                "{n} {ty} {prims} render nothing: crust does not read the {ty} type \
                 (the first at {first})"
            );
        }
    }
}

impl ImportCaches<'_> {
    /// How many native placements `proto_path` has on the stage.
    pub(in crate::scene) fn placement_count(&self, proto_path: &sdf::Path) -> u32 {
        self.placements
            .get(proto_path.as_str())
            .copied()
            .unwrap_or(0)
    }
}

impl<'a> ImportCaches<'a> {
    pub(in crate::scene) fn new(
        assets: &'a dyn AssetLoader,
        host: &'a dyn index::host::HostFacts,
        stage_path: &'a Path,
        subdiv: SubdivPolicy,
        working: crate::color::Space,
        library: Arc<crust_mtlx::Library>,
    ) -> Self {
        ImportCaches {
            materials: MaterialCache::new(library),
            meshes: MeshArena {
                working,
                ..MeshArena::new(subdiv)
            },
            protos: HashMap::new(),
            groups: HashMap::new(),
            prototypes: HashMap::new(),
            placements: HashMap::new(),
            assets,
            host,
            stage_path,
            asset_time: Duration::ZERO,
            ies: HashMap::new(),
            light_textures: HashMap::new(),
            environments: None,
            failed_assets: std::collections::HashSet::new(),
            implicit: ImplicitPolicy::default(),
            unread: UnreadGprims::default(),
            motion: TransformMotion::default(),
            working,
            luma: crate::color::luma(working),
            time: None,
            scene: None,
            top: None,
            inst: None,
        }
    }

    /// The prim at `path` of the index being committed.
    fn prim_data(&self, path: &sdf::Path) -> index::data::PrimData {
        match &self.scene {
            Some(scene) => index::data::PrimData::in_index(scene, path),
            None => unreachable!("a reader reached outside a commit of the scene index"),
        }
    }
}
