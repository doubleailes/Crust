//! `UsdRenderSettings` and the `crust:*` render attributes → [`RenderSettings`], and
//! which camera the render was told to use.

use crate::warning;
use moho::openusd::sdf;
use moho::scene_index::SceneIndex;
use moho::tokens::prim_type;
use tracing::debug;

use crate::color::Space;
use crate::filter::PixelFilter;
use crate::light::LightSelection;
use crate::sampler::SamplerKind;
use crate::tracer::{RenderSettings, SamplingStrategy};

use moho::tokens::render as rt;

use super::index::data::{PrimData, active_render_settings};
use super::index::fields::Fields;
use super::index::stage_facts;

const DEFAULT_GUIDING_TRAIN_ITERATIONS: u32 = 4;
const DEFAULT_GUIDING_PROB: f32 = 0.5;
/// Hydra's default for `domeLightCameraVisibility`: the camera sees domes.
const DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY: bool = true;
/// Hydra's default for `enableExposureCompensation`: the camera's exposure
/// applies.
const DEFAULT_ENABLE_EXPOSURE_COMPENSATION: bool = true;

/// A camera the render was told to use, and who said so — which decides what
/// happens if it is not on the stage.
pub(super) enum CameraChoice {
    /// The host's `UsdImportOptions::camera` (the CLI's `--camera`). Missing
    /// is an error.
    Requested(sdf::Path),
    /// The stage's `RenderSettings.camera` relationship. Missing warns and
    /// falls back to the first camera met.
    Settings(sdf::Path),
}

impl CameraChoice {
    pub(super) fn path(&self) -> &sdf::Path {
        match self {
            CameraChoice::Requested(p) | CameraChoice::Settings(p) => p,
        }
    }
}

/// The camera a render is told to use: the host's `requested` path (the
/// CLI's `--camera`), else the first RenderProduct's camera (`product_camera`,
/// which is the settings' own when the product names none), else the stage's
/// `RenderSettings.camera`. Read off the payload-free scene, before any
/// traversal meets a camera. The one precedence the import and `crust ls`
/// both follow.
pub(super) fn wanted_camera(
    requested: Option<sdf::Path>,
    product_camera: Option<sdf::Path>,
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<CameraChoice> {
    match requested {
        Some(p) => Some(CameraChoice::Requested(p)),
        None => product_camera
            .or_else(|| render_settings_camera(scene, pass))
            .map(CameraChoice::Settings),
    }
}

/// Which camera a render goes through, once the walk is done.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CameraPick<'a> {
    /// The camera it was told to use.
    Wanted,
    /// The first camera the import's walk met: nothing was named, or the
    /// stage's `RenderSettings.camera` names a prim that is not a camera.
    First,
    /// The procedural fallback camera: the stage has none.
    Procedural,
    /// The host asked for a camera the stage does not have: an error.
    Missing(&'a sdf::Path),
}

/// The fallback rule of [`CameraChoice`]: `wanted_met` says whether the
/// wanted camera was met, `any_met` whether any camera was. Shared by the
/// import (`resolve_camera`) and the listing (`is_render_camera`), so the
/// camera `ls` marks is the one a render uses.
pub(super) fn pick_camera(
    wanted: Option<&CameraChoice>,
    wanted_met: bool,
    any_met: bool,
) -> CameraPick<'_> {
    match wanted {
        Some(_) if wanted_met => CameraPick::Wanted,
        Some(CameraChoice::Requested(p)) => CameraPick::Missing(p),
        _ if any_met => CameraPick::First,
        _ => CameraPick::Procedural,
    }
}

impl std::fmt::Display for CameraChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CameraChoice::Requested(p) => write!(f, "camera {p} (requested)"),
            CameraChoice::Settings(p) => write!(f, "camera {p} (RenderSettings.camera)"),
        }
    }
}

/// The prim holding the render settings: the active `pass`'s `renderSource`
/// target when it is a `RenderSettings` prim, else the one the stage's
/// `renderSettingsPrimPath` metadatum names, else the conventional
/// `/Render/settings`. The one answer every settings reader, the products
/// and the provenance take, so a pass's own settings reach all of them.
/// [`check_render_source`] warns about a target this passes over.
///
/// Read from the scene index: the pass and the settings prim by their Hydra
/// types, `renderSettingsPrimPath` from the root's `sceneGlobals`.
pub(super) fn render_settings_path(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<sdf::Path> {
    pass.and_then(|p| render_source(scene, p))
        .filter(|target| is_render_settings(scene, target))
        .or_else(|| stage_settings_path(scene).or_else(|| sdf::path("/Render/settings").ok()))
}

/// The stage's `renderSettingsPrimPath` metadatum as a path, when it names
/// one: the root's `sceneGlobals/activeRenderSettingsPrim`.
fn stage_settings_path(scene: &dyn SceneIndex) -> Option<sdf::Path> {
    active_render_settings(scene)
}

/// The `RenderSettings` prim the render reads: [`render_settings_path`] when a
/// settings prim is there, `None` when the stage has none and the render runs
/// on the defaults. What `crust check` reports; the provenance keeps the path
/// itself, so an edit that authors settings there is still a settings edit.
pub(super) fn read_settings_prim(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<sdf::Path> {
    render_settings_path(scene, pass).filter(|p| is_render_settings(scene, p))
}

/// The first target of `pass`'s `renderSource` relationship, if it authors one.
pub(super) fn render_source(scene: &dyn SceneIndex, pass: &sdf::Path) -> Option<sdf::Path> {
    let prim = PrimData::at(scene, pass);
    if prim.ty() != prim_type::RENDER_PASS {
        return None;
    }
    Fields::of(&prim, [rt::RENDER_PASS])
        .paths(rt::RENDER_SOURCE)
        .into_iter()
        .next()
}

fn is_render_settings(scene: &dyn SceneIndex, path: &sdf::Path) -> bool {
    path.is_prim() && PrimData::at(scene, path).ty() == prim_type::RENDER_SETTINGS
}

/// Checks the host's `pass` names a `RenderPass` prim on `stage`, and warns
/// when its `renderSource` targets something [`render_settings_path`] passes
/// over. Run once per import, on the payload-free scene, before anything is
/// read.
pub(super) fn check_pass(
    scene: &dyn SceneIndex,
    stage: &moho::openusd::usd::Stage,
    pass: &sdf::Path,
) -> Result<(), crate::Error> {
    if PrimData::at(scene, pass).ty() != prim_type::RENDER_PASS {
        return Err(crate::Error::PassNotFound {
            path: pass.to_string(),
            why: stage_facts::not_a_pass(stage, pass),
        });
    }
    if let Some(target) = render_source(scene, pass)
        && !is_render_settings(scene, &target)
    {
        warning!(
            RenderPassRenderSourceInvalid,
            at = pass,
            "{pass}: renderSource targets {target}, which is not a RenderSettings prim — \
             reading the stage's render settings"
        );
    }
    Ok(())
}

/// The render settings prim the render reads ([`render_settings_path`]) and
/// its flattened `renderSettings` (`HdRenderSettingsSchema`); `None` when
/// there is no settings prim.
pub(super) fn settings_fields(scene: &dyn SceneIndex, pass: Option<&sdf::Path>) -> Option<Fields> {
    let prim = PrimData::at(scene, &render_settings_path(scene, pass)?);
    (prim.ty() == prim_type::RENDER_SETTINGS).then(|| Fields::of(&prim, [rt::RENDER_SETTINGS]))
}

/// The render settings' `namespacedSettings`: every namespaced attribute the
/// prim authors (Crust's `crust:*` settings), by full name. Empty without a
/// settings prim.
pub(super) fn namespaced_settings(scene: &dyn SceneIndex, pass: Option<&sdf::Path>) -> Fields {
    match settings_fields(scene, pass) {
        Some(fields) => fields.child(rt::NAMESPACED_SETTINGS),
        None => Fields::empty(&sdf::Path::abs_root()),
    }
}

/// The first target of the render settings' `camera` relationship — the USD
/// way for a stage to say which of its cameras is the shot camera.
pub(super) fn render_settings_camera(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<sdf::Path> {
    settings_fields(scene, pass)?
        .paths(rt::CAMERA)
        .into_iter()
        .next()
}

/// Whether the camera sees the lights at infinity at all:
/// `crust:domeLightCameraVisibility` in the render settings'
/// `namespacedSettings` (Hydra's own `domeLightCameraVisibility` is a
/// renderer setting the application gives, not stage data). Default `true`. `false` hides every dome,
/// distant light and backdrop from camera rays whatever the lights themselves
/// author; how they illuminate is unchanged.
pub(super) fn dome_light_camera_visibility(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> bool {
    namespaced_settings(scene, pass)
        .bool("crust:domeLightCameraVisibility")
        .unwrap_or(DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY)
}

/// Whether the render camera's exposure scales the image:
/// `crust:enableExposureCompensation` in the render settings'
/// `namespacedSettings` (Hydra's own `enableExposureCompensation`, which
/// hdEmbree reads, is a renderer setting, not stage data). Default `true`. `false` renders at a scale
/// of 1 whatever the camera authors.
pub(super) fn enable_exposure_compensation(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> bool {
    namespaced_settings(scene, pass)
        .bool("crust:enableExposureCompensation")
        .unwrap_or(DEFAULT_ENABLE_EXPOSURE_COMPENSATION)
}

/// `crust:subdivisionLevel` as authored on the `RenderSettings` prim — the
/// stage's refinement level for every mesh whose `subdivisionScheme` is
/// not `none`. `None` when unauthored; the caller
/// ([`resolve_subdiv_level`](super::attrs::resolve_subdiv_level)) applies the
/// host override, the default and the clamp. A geometry setting, not a
/// tracer one, so it stays out of [`RenderSettings`].
pub(super) fn render_settings_subdiv_level(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<i32> {
    namespaced_settings(scene, pass).i32("crust:subdivisionLevel")
}

/// `crust:subdivisionEdgeLength` as authored on the `RenderSettings` prim: the
/// stage's adaptive-subdivision target, in pixels. `None` when unauthored; the
/// caller ([`resolve_subdiv_edge_length`](super::attrs::resolve_subdiv_edge_length))
/// applies the host override and refuses a bad value. A geometry setting, like
/// [`render_settings_subdiv_level`].
pub(super) fn render_settings_subdiv_edge_length(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<f32> {
    namespaced_settings(scene, pass).f32("crust:subdivisionEdgeLength")
}

/// `crust:dicingCamera` on the `RenderSettings` prim, a `string` holding an
/// absolute prim path (design D5: Hydra carries no namespaced relationship):
/// the camera tessellation reads (adaptive subdivision, implicit shapes sized
/// on screen) in place of the render camera. Composition does not remap a
/// string; `crust check` says so where the prim comes through an arc
/// (`settings.path_across_arc`). `None` when unauthored; the caller checks
/// the target is a camera. A geometry setting, like
/// [`render_settings_subdiv_level`].
pub(super) fn render_settings_dicing_camera(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Option<sdf::Path> {
    namespaced_settings(scene, pass)
        .path_strings("crust:dicingCamera")
        .into_iter()
        .next()
}

/// The working colour space the stage asks for: `renderingColorSpace` on the
/// `RenderSettings` prim, resolved through the OCIO config ([`crate::color`]).
/// Unauthored or empty is `lin_rec709`; a name the config does not know, or a
/// space that is not scene-linear, is refused with a warning and is
/// `lin_rec709` too.
pub(super) fn render_settings_color_space(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> Space {
    let Some(fields) = settings_fields(scene, pass) else {
        return Space::LIN_REC709;
    };
    let path = fields.path().clone();
    match fields.text(rt::RENDERING_COLOR_SPACE) {
        Some(name) if !name.is_empty() => {
            crate::color::working_space(&name).unwrap_or_else(|why| {
                warning!(
                    ColorWorkingSpaceRefused,
                    at = path,
                    "{path}: renderingColorSpace refused ({why}); rendering in lin_rec709"
                );
                Space::LIN_REC709
            })
        }
        _ => Space::LIN_REC709,
    }
}

pub(super) fn import_render_settings(
    scene: &dyn SceneIndex,
    pass: Option<&sdf::Path>,
) -> RenderSettings {
    let Some(path) = render_settings_path(scene, pass) else {
        return default_settings();
    };

    let settings = PrimData::at(scene, &path);
    if settings.ty() != prim_type::RENDER_SETTINGS {
        debug!(
            "No UsdRenderSettings at {} — using defaults for render settings",
            path
        );
        return default_settings();
    }
    // Crust's own settings, as Hydra carries them.
    let prim = Fields::of(&settings, [rt::RENDER_SETTINGS]).child(rt::NAMESPACED_SETTINGS);

    let d = RenderSettings::default();
    let (mut w, mut h) = d.get_dimensions();
    // The settings' own resolution, UsdImaging's `__usdRenderSettings` (its
    // schema fallback included); the first product's replaces it. A side that
    // is not positive is refused rather than cast: `-1 as usize` is a frame no
    // camera can be conformed to, nor any buffer allocated for.
    if let Some(v2) = Fields::of(&settings, [rt::USD_RENDER_SETTINGS])
        .value(rt::RESOLUTION)
        .and_then(|v| v.try_as_vec_2i())
    {
        if v2.x > 0 && v2.y > 0 {
            w = v2.x as usize;
            h = v2.y as usize;
        } else {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "resolution = ({}, {}) is not a positive size — using ({w}, {h})",
                v2.x,
                v2.y
            );
        }
    }

    // Custom `crust:*` attrs. We look them up on the RenderSettings prim.
    let spp = prim
        .i32("crust:samplesPerPixel")
        .map_or(d.samples_per_pixel(), |n| n as u32);
    // `crust:maxDepth` counts shaded path vertices, not bounces: 1 is the
    // camera hit alone (visible emission and direct lighting), and 0 would
    // shade nothing — a black frame. Both 0 and a negative depth (which a
    // cast would wrap to `u32::MAX`) are refused.
    let max_depth = match prim.i32("crust:maxDepth") {
        Some(n) if n < 1 => {
            let fallback = d.max_depth();
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:maxDepth = {n} would shade no vertex (1 is direct lighting only) \
                 — using {fallback}"
            );
            fallback
        }
        Some(n) => n as u32,
        None => d.max_depth(),
    };
    // A negative minimum is refused rather than cast: `-1 as u32` is
    // `u32::MAX`, which would overflow the first check point.
    let min_spp = match prim.i32("crust:minSamplesPerPixel") {
        Some(n) if n < 0 => {
            let fallback = d.min_samples_per_pixel();
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:minSamplesPerPixel = {n} is negative — using {fallback}"
            );
            fallback
        }
        Some(n) => n as u32,
        None => d.min_samples_per_pixel(),
    };
    let variance = prim
        .f32("crust:varianceThreshold")
        .unwrap_or(d.variance_threshold());
    let frame = prim.i32("crust:frame").map_or(d.frame(), |n| n as isize);

    // Path guiding (opt-in).
    let guiding = prim.bool("crust:pathGuiding").unwrap_or(false);
    let guiding_iters = prim
        .i32("crust:guidingTrainIterations")
        .unwrap_or(DEFAULT_GUIDING_TRAIN_ITERATIONS as i32)
        .max(1) as u32;
    let guiding_prob = prim
        .f32("crust:guidingProb")
        .unwrap_or(DEFAULT_GUIDING_PROB);

    // MIS strategy: `power` (default) | `balance` | `light` | `bsdf`.
    let strategy = match prim.text("crust:samplingStrategy") {
        None => SamplingStrategy::PowerMis,
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:samplingStrategy: {e} — using power MIS"
            );
            SamplingStrategy::PowerMis
        }),
    };

    // Light selection: `power` (default) | `uniform` | `learned` | `neural`.
    let light_selection = match prim.text("crust:lightSelection") {
        None => LightSelection::Power,
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:lightSelection: {e} — picking lights by power"
            );
            LightSelection::Power
        }),
    };

    // Pixel reconstruction filter: `box` | `triangle` (default) | `gaussian`
    // | `blackman` | `mitchell`, each at its conventional radius unless
    // `crust:pixelFilterRadius` overrides it (in pixels, from the center).
    let mut filter = match prim.text("crust:pixelFilter") {
        None => PixelFilter::default(),
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:pixelFilter: {e} — using the triangle filter"
            );
            PixelFilter::default()
        }),
    };
    if let Some(radius) = prim.f32("crust:pixelFilterRadius") {
        filter = filter.with_radius(radius);
    }
    // The QMC sampler: `sobol` (default) | `sobolBn` | `lattice` |
    // `latticeBn` | `pmj` | `pmjBn`.
    let sampler = match prim.text("crust:sampler") {
        None => SamplerKind::default(),
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:sampler: {e} — using sobol"
            );
            SamplerKind::default()
        }),
    };
    // Firefly clamp on indirect light, in the film's linear units (the
    // largest channel of one sample's indirect radiance). Unauthored takes
    // the engine default; an authored 0 leaves the estimator unbiased.
    let indirect_clamp = prim
        .f32("crust:indirectClamp")
        .unwrap_or(crate::tracer::DEFAULT_INDIRECT_CLAMP);
    // Adaptive sampling's cross-neighbour tolerance, in convergence-index
    // units; negative turns the comparison off. A non-finite value cannot
    // be compared against, so it is refused rather than silently disabling
    // the guard.
    let neighbour_tolerance = match prim.f32("crust:adaptiveNeighbourTolerance") {
        Some(t) if t.is_finite() => t,
        Some(t) => {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:adaptiveNeighbourTolerance = {t} is not finite — using {}",
                crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
            );
            crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
        }
        None => crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE,
    };

    // Light samples per vertex: at the camera vertex and at every later one.
    // Fewer than one is no estimator at all, so it is refused rather than
    // clamped without a word.
    let light_samples = light_sample_count(&prim, "crust:lightSamples");
    let light_samples_indirect = light_sample_count(&prim, "crust:lightSamplesIndirect");

    // What the stage asked for, before the CLI's own overrides. Every field
    // here silently falls back to a default when unauthored, so this is the
    // line that separates "the scene set it" from "nobody did".
    debug!(
        "RenderSettings at {}: {w}x{h}, {spp} spp (min {min_spp}, variance threshold \
         {variance}, neighbour tolerance {neighbour_tolerance}), max depth {max_depth}, \
         frame {frame}, strategy {strategy:?}, \
         light selection {light_selection:?}, light samples {light_samples} camera / \
         {light_samples_indirect} indirect, filter {} radius {}, indirect clamp {}, guiding {}",
        prim.path(),
        filter.name(),
        filter.radius(),
        if indirect_clamp > 0.0 {
            indirect_clamp.to_string()
        } else {
            "off".to_string()
        },
        if guiding {
            format!("on ({guiding_iters} training iterations, guide probability {guiding_prob})")
        } else {
            "off".to_string()
        }
    );
    d.with_resolution(w, h)
        .with_max_depth(max_depth)
        .with_adaptive_sampling(min_spp, variance)
        .with_frame(frame)
        .with_samples_per_pixel(spp)
        .with_guiding(guiding, guiding_iters, guiding_prob)
        .with_sampling_strategy(strategy)
        .with_light_selection(light_selection)
        .with_pixel_filter(filter)
        .with_sampler(sampler)
        .with_indirect_clamp(indirect_clamp)
        .with_adaptive_neighbour_tolerance(neighbour_tolerance)
        .with_light_samples(light_samples, light_samples_indirect)
}

/// A per-vertex light sample count off the `RenderSettings` prim: the
/// authored value when it is at least 1, clamped to `MAX_LIGHT_SAMPLES` with
/// a warning above it (a count multiplies every vertex's shadow rays, so a
/// typo there is a render that never ends), else the default with a warning.
/// Unauthored is the default.
fn light_sample_count(prim: &Fields, name: &str) -> u32 {
    use crate::tracer::{DEFAULT_LIGHT_SAMPLES, MAX_LIGHT_SAMPLES};
    match prim.i32(name) {
        Some(n) if n >= 1 && n as u32 > MAX_LIGHT_SAMPLES => {
            warning!(
                SettingsLightSamplesClamped,
                at = prim.path(),
                "{name} = {n} is above {MAX_LIGHT_SAMPLES} — taking {MAX_LIGHT_SAMPLES}"
            );
            MAX_LIGHT_SAMPLES
        }
        Some(n) if n >= 1 => n as u32,
        Some(n) => {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "{name} = {n} is below 1 — taking {DEFAULT_LIGHT_SAMPLES}"
            );
            DEFAULT_LIGHT_SAMPLES
        }
        None => DEFAULT_LIGHT_SAMPLES,
    }
}

/// Warns when `time` lies outside the stage's authored
/// `startTimeCode`..`endTimeCode`. Not an error — USD holds the first or last
/// sample past either end, so the render is well defined — but a frame off
/// the end of the shot is far more often a typo than a request, and it would
/// otherwise render a plausible, frozen image without a word.
pub(super) fn check_time_range(range: Option<(f64, f64)>, time: f64) {
    let Some((start, end)) = range else {
        debug!("Rendering at time code {time}; the stage authors no startTimeCode/endTimeCode");
        return;
    };
    if time < start || time > end {
        warning!(
            TimeOutsideRange,
            "Frame {time} is outside the stage's time range [{start}, {end}]; \
             animated attributes hold their nearest time sample"
        );
    } else {
        debug!("Rendering at time code {time} of [{start}, {end}]");
    }
}

fn default_settings() -> RenderSettings {
    RenderSettings::default()
}
