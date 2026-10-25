//! `UsdGeomCamera` → [`Camera`].

use glam::{Mat4 as GMat4, Vec3, Vec3A};
use moho::openusd::sdf;
use moho::openusd::usd::{Prim, Stage};
use moho::scene_index::SceneIndex;
use moho::tokens::prim_type;
use openusd_schemas::render::{AspectRatioConformPolicy, conform_aperture};
use tracing::debug;

use crate::camera::{Camera, MAX_CLIP_PLANES};
use crate::tracer::RenderSettings;
use crate::warning;

use super::attrs::decode_text;
use super::index::bridge;
use super::index::data::{PrimData, stage_metadata};
use super::prim_at;
use super::products::{Framing, valid_pixel_aspect};
use super::xform::compose_with_parent;

/// One `UsdGeomCamera` as all its readers see it: [`build_camera`], which
/// builds the render camera, [`screen_projection`], which adaptive
/// subdivision and on-screen implicit shapes read before the traversal, and
/// [`camera_lens`], which `crust ls camera --json` reports. All derive from
/// this one read — the conform included — so they cannot describe two
/// different cameras.
struct CameraFrame {
    /// The camera's position and its view direction and up, in world space.
    /// USD's camera looks down local −Z with +Y up.
    eye: Vec3,
    forward: Vec3,
    up: Vec3,
    /// `projection = "orthographic"`: the rays are parallel, and the
    /// apertures are the view window in tenths of a scene unit; the focal
    /// length plays no part.
    orthographic: bool,
    /// The focal length and the apertures, in the same units.
    focal_length: f32,
    /// Horizontal, vertical, as authored; unauthored, the schema's fallbacks
    /// (20.955 × 15.2908).
    aperture: [f32; 2],
    /// `aperture` conformed to the image's aspect by the render's
    /// `aspectRatioConformPolicy` ([`conform`]): the aperture the render
    /// frames.
    conformed: [f32; 2],
    /// `horizontalApertureOffset`, `verticalApertureOffset`: where the centre
    /// of the film back sits off the optical axis, in the apertures' units.
    /// The conform leaves them alone, as OpenUSD's `CameraUtilConformWindow`
    /// does: it resizes the window about its centre, which is the offset.
    offset: [f32; 2],
    /// The width of a pixel over its height: the render's
    /// `pixelAspectRatio`, or under `adjustPixelAspectRatio` the one the
    /// aperture needs ([`conform`]). `conformed` has the aspect
    /// `pixel_aspect · width / height`.
    pixel_aspect: f32,
    /// The image size the aperture is conformed to.
    width: f32,
    height: f32,
}

/// The camera at `path` in `scene`, when the prim there is one.
fn camera_at(scene: &dyn SceneIndex, path: &sdf::Path) -> Option<PrimData> {
    let cam = PrimData::at(scene, path);
    (cam.ty() == prim_type::CAMERA).then_some(cam)
}

impl CameraFrame {
    /// `cam`'s frame, its world transform `world` (the transforms still read
    /// through the bridge until they move to the index, `moho-import` 5.1).
    fn read(cam: &PrimData, world: GMat4, settings: &RenderSettings, framing: Framing) -> Self {
        let focal_length = cam.custom_f32("focalLength").unwrap_or(50.0);
        let horiz_aperture = cam.custom_f32("horizontalAperture").unwrap_or(20.955);
        let (w, h) = settings.get_dimensions();
        let (width, height) = (w as f32, h as f32);
        // An unauthored vertical aperture is the schema's fallback, conformed
        // like any other: under `expandAperture` an image wider than
        // 20.955 × 15.2908 widens the horizontal aperture, as USD frames it.
        let vert_aperture = cam
            .custom_f32("verticalAperture")
            .unwrap_or(FALLBACK_VERTICAL_APERTURE);
        let aperture = [horiz_aperture, vert_aperture];
        let (conformed, pixel_aspect) =
            conform(framing.policy, (w, h), framing.pixel_aspect, aperture);
        // USD reads a token that names no projection as perspective.
        let orthographic = cam
            .value("projection")
            .and_then(decode_text)
            .is_some_and(|p| p == "orthographic");
        let offset = [
            cam.custom_f32("horizontalApertureOffset").unwrap_or(0.0),
            cam.custom_f32("verticalApertureOffset").unwrap_or(0.0),
        ];
        CameraFrame {
            eye: world.transform_point3(Vec3::ZERO),
            forward: world.transform_vector3(Vec3::NEG_Z).normalize(),
            up: world.transform_vector3(Vec3::Y).normalize(),
            orthographic,
            focal_length,
            aperture,
            conformed,
            offset,
            pixel_aspect,
            width,
            height,
        }
    }

    /// The conformed vertical aperture, which the field of view is derived
    /// from.
    fn vert_aperture(&self) -> f32 {
        self.conformed[1]
    }

    /// Tangent of the half vertical field of view.
    fn tan_half_vfov(&self) -> f32 {
        self.vert_aperture() / (2.0 * self.focal_length)
    }

    /// The image window's width over its height: the image's aspect in
    /// pixels times the pixel aspect, which the conformed aperture has. With
    /// square pixels exactly `width / height`.
    fn aspect(&self) -> f32 {
        self.width / self.height * self.pixel_aspect
    }

    /// An orthographic camera's view window, width and height in scene
    /// units: the conformed apertures, which USD measures in tenths of a
    /// scene unit (`GfCamera::APERTURE_UNIT`).
    fn ortho_window(&self) -> [f32; 2] {
        self.conformed.map(|a| a * ORTHO_APERTURE_UNIT)
    }

    /// An orthographic camera's window centre off the view axis, horizontal
    /// and vertical in scene units: the offsets, in the apertures' tenths.
    fn ortho_centre(&self) -> [f32; 2] {
        self.offset.map(|o| o * ORTHO_APERTURE_UNIT)
    }

    /// How far the offsets move the image window, in frames of the conformed
    /// aperture: the shift [`Camera::with_window_shift`] takes. An offset
    /// over a degenerate aperture moves nothing.
    fn window_shift(&self) -> [f32; 2] {
        let shift = |offset: f32, aperture: f32| {
            let s = offset / aperture;
            if s.is_finite() { s } else { 0.0 }
        };
        [
            shift(self.offset[0], self.conformed[0]),
            shift(self.offset[1], self.conformed[1]),
        ]
    }
}

/// `UsdGeomCamera`'s fallback `verticalAperture`, in mm.
const FALLBACK_VERTICAL_APERTURE: f32 = 15.2908;

/// Scene units per unit of an orthographic camera's aperture.
const ORTHO_APERTURE_UNIT: f32 = 0.1;

/// How far two aspect ratios may differ, relatively, and still be the same
/// shape: an aperture this close to the image's is not conformed, so a
/// rounding difference cannot move the field of view by an ulp.
const SAME_ASPECT: f32 = 1e-6;

/// `aperture` (horizontal, vertical) conformed by `policy` to a `width` ×
/// `height` image of pixels `pixel_aspect` times wider than they are tall,
/// as OpenUSD's `CameraUtilConformWindow` conforms it, and the pixel aspect
/// the render then has. Every policy but `adjustPixelAspectRatio` keeps
/// `pixel_aspect` and resizes the aperture; that one keeps the aperture and
/// returns the pixel aspect it needs, `aperture aspect / (width / height)`
/// (`conform_aperture`'s own).
///
/// An aperture whose aspect already is the image's (`pixel_aspect · width /
/// height`) is returned unchanged with `pixel_aspect`, by every policy. The
/// conformed aperture has the aspect `returned pixel aspect · width /
/// height`, which the ray generator takes ([`CameraFrame::aspect`]).
fn conform(
    policy: AspectRatioConformPolicy,
    (width, height): (usize, usize),
    pixel_aspect: f32,
    aperture: [f32; 2],
) -> ([f32; 2], f32) {
    let image_aspect = width as f32 / height as f32 * pixel_aspect;
    let aperture_aspect = aperture[0] / aperture[1];
    if ((aperture_aspect / image_aspect) - 1.0).abs() <= SAME_ASPECT {
        return (aperture, pixel_aspect);
    }
    let resolution = [
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    ];
    let conformed = conform_aperture(policy, resolution, pixel_aspect, aperture);
    let (size, pixel_aspect) = (conformed.aperture_size, conformed.pixel_aspect_ratio);
    // A pixel aspect `adjustPixelAspectRatio` derived that no EXR can hold
    // (an aperture over a million times wider than the image, or a
    // degenerate one, whose ratio is no number): framed as the closest
    // square-pixel frame that keeps the whole aperture, `expandAperture`.
    if !valid_pixel_aspect(pixel_aspect) {
        let size = conform_aperture(
            AspectRatioConformPolicy::ExpandAperture,
            resolution,
            1.0,
            aperture,
        )
        .aperture_size;
        return (size, 1.0);
    }
    // `conform_aperture` leaves a degenerate aperture alone; any other comes
    // out at the image's aspect, or the rays would stretch it.
    let image_aspect = width as f32 / height as f32 * pixel_aspect;
    debug_assert!(
        !(aperture.iter().all(|a| a.is_finite() && *a > 0.0) && image_aspect.is_finite())
            || ((size[0] / size[1]) / image_aspect - 1.0).abs() <= 1e-5,
        "conformed aperture {size:?} is not at the image's aspect {image_aspect}"
    );
    (size, pixel_aspect)
}

/// A camera's lens as the render reads it, unauthored values at the render's
/// fallbacks — what [`build_camera`] builds from, and what `crust ls camera
/// --json` reports.
pub(super) struct CameraLens {
    pub(super) focal_length: f32,
    /// Horizontal, vertical, as authored; unauthored, the schema's fallbacks
    /// (20.955 × 15.2908).
    pub(super) aperture: [f32; 2],
    /// `aperture` conformed to the image by the render's
    /// `aspectRatioConformPolicy`: the aperture the render frames.
    pub(super) conformed_aperture: [f32; 2],
    /// Horizontal, vertical, as authored (0 unauthored); not conformed.
    pub(super) aperture_offset: [f32; 2],
    /// `0` is a pinhole.
    pub(super) f_stop: f32,
    pub(super) focus_distance: f32,
}

/// The lens of the camera at `prim`, read as [`build_camera`] reads it.
pub(super) fn camera_lens(
    scene: &dyn SceneIndex,
    stage: &Stage,
    path: &sdf::Path,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<CameraLens> {
    let cam = camera_at(scene, path)?;
    let world = local_to_world(stage, &bridge::prim(stage, path));
    Some(lens(
        &cam,
        &CameraFrame::read(&cam, world, settings, framing),
    ))
}

fn lens(cam: &PrimData, frame: &CameraFrame) -> CameraLens {
    CameraLens {
        focal_length: frame.focal_length,
        aperture: frame.aperture,
        conformed_aperture: frame.conformed,
        aperture_offset: frame.offset,
        f_stop: cam.custom_f32("fStop").unwrap_or(0.0),
        focus_distance: cam.custom_f32("focusDistance").unwrap_or(10.0),
    }
}

/// A camera as the import builds it: the rays it generates, and its linear
/// exposure scale as authored. The scale is checked only once this camera is
/// the one rendered through and its exposure applies, so a fallback candidate
/// or a disabled exposure raises no warning; so is the stage unit its lens
/// refused ([`RenderCamera::warn_lens_units`]).
pub(super) struct RenderCamera {
    pub(super) camera: Camera,
    pub(super) exposure_scale: f32,
    /// The stage's `metersPerUnit` when the lens read it and refused it (not
    /// finite, not positive, or overflowing the diameter); the lens was built
    /// with the fallback.
    pub(super) refused_meters_per_unit: Option<f64>,
    /// Its shutter, for the check that the one motion was measured over is
    /// this camera's ([`Shutter`]).
    pub(super) shutter: ShutterOffsets,
    /// Its lens, for the stamp; `None` for the procedural camera.
    pub(super) lens: Option<crate::scene::Lens>,
    /// What of its clipping was refused or dropped.
    pub(super) clipping: ClippingRefusals,
}

/// What [`clipping`] refused of a camera's `clippingRange` and
/// `clippingPlanes`, for the warnings raised once it is the camera rendered
/// through ([`RenderCamera::warn_clipping`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ClippingRefusals {
    /// The range as authored, when it is not `0 ≤ near < far` (finite near);
    /// the camera clips to no range.
    pub(super) range: Option<[f32; 2]>,
    /// Planes with a component that is not finite, each ignored.
    pub(super) non_finite_planes: usize,
    /// Planes past [`MAX_CLIP_PLANES`], ignored.
    pub(super) dropped_planes: usize,
}

/// A camera's clipping as the render honours it: the near and far distances
/// (`0` and `∞` when `clippingRange` is unauthored or refused) and the
/// finite planes, at most [`MAX_CLIP_PLANES`] of them.
///
/// An unauthored `clippingRange` clips nothing rather than reading USD's
/// fallback `(1, 1000000)`: crust rendered every scene unclipped before it
/// read the attribute, and a stage in metres with its camera within a metre
/// of a set, or one whose geometry reaches past a million units, would
/// change with no edit. Every DCC exporter authors the range, so a camera
/// from one clips as it does in usdview.
struct Clipping {
    near: f32,
    far: f32,
    planes: Vec<[f32; 4]>,
    refusals: ClippingRefusals,
}

fn clipping(cam: &PrimData) -> Clipping {
    let mut refusals = ClippingRefusals::default();
    let range = cam.value("clippingRange").and_then(|v| match v {
        sdf::Value::Vec2f(r) => Some([r.x, r.y]),
        sdf::Value::Vec2d(r) => Some([r.x as f32, r.y as f32]),
        _ => None,
    });
    let (near, far) = match range {
        // `far` may be infinite (no far distance); `!(far > near)` also
        // refuses a NaN.
        Some([near, far]) if near.is_finite() && near >= 0.0 && far > near => (near, far),
        Some(r) => {
            refusals.range = Some(r);
            (0.0, f32::INFINITY)
        }
        None => (0.0, f32::INFINITY),
    };
    let authored: Vec<[f32; 4]> = match cam.value("clippingPlanes") {
        Some(sdf::Value::Vec4fVec(v)) => v.iter().map(|p| [p.x, p.y, p.z, p.w]).collect(),
        Some(sdf::Value::Vec4dVec(v)) => v
            .iter()
            .map(|p| [p.x as f32, p.y as f32, p.z as f32, p.w as f32])
            .collect(),
        _ => Vec::new(),
    };
    let mut planes = Vec::new();
    for plane in authored {
        if !plane.iter().all(|c| c.is_finite()) {
            refusals.non_finite_planes += 1;
        } else if planes.len() == MAX_CLIP_PLANES {
            refusals.dropped_planes += 1;
        } else {
            planes.push(plane);
        }
    }
    Clipping {
        near,
        far,
        planes,
        refusals,
    }
}

impl RenderCamera {
    /// Raises `camera.invalid_meters_per_unit` for the camera rendered
    /// through, when its lens refused the stage's unit.
    pub(super) fn warn_lens_units(&self, camera_path: Option<&str>) {
        if let Some(m) = self.refused_meters_per_unit {
            let camera = camera_path.unwrap_or("the render camera");
            warning!(
                CameraInvalidMetersPerUnit,
                "{camera}: the stage's metersPerUnit = {m} is not a length the lens can use; it \
                 reads it as {FALLBACK_METERS_PER_UNIT} (a centimetre)"
            );
        }
    }

    /// Raises `camera.invalid_clipping` and `camera.too_many_clipping_planes`
    /// for the camera rendered through, for what of its clipping was
    /// refused or dropped ([`ClippingRefusals`]).
    pub(super) fn warn_clipping(&self, camera_path: Option<&str>) {
        let camera = camera_path.unwrap_or("the render camera");
        let ClippingRefusals {
            range,
            non_finite_planes,
            dropped_planes,
        } = &self.clipping;
        if let Some([near, far]) = range {
            warning!(
                CameraInvalidClipping,
                at = camera,
                "{camera}: clippingRange = ({near}, {far}) is not 0 <= near < far; the camera \
                 clips to no range"
            );
        }
        if *non_finite_planes > 0 {
            warning!(
                CameraInvalidClipping,
                at = camera,
                "{camera}: {non_finite_planes} of its clippingPlanes have a component that is \
                 not finite; they are ignored"
            );
        }
        if *dropped_planes > 0 {
            warning!(
                CameraTooManyClippingPlanes,
                at = camera,
                "{camera}: clippingPlanes authors {} planes; crust honours the first \
                 {MAX_CLIP_PLANES} and ignores the other {dropped_planes}",
                MAX_CLIP_PLANES + dropped_planes
            );
        }
    }
}

/// The camera at `prim` and its exposure scale, `None` when `prim` is not a
/// camera on `stage`.
pub(super) fn build_camera(
    scene: &dyn SceneIndex,
    stage: &Stage,
    path: &sdf::Path,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<RenderCamera> {
    let cam = camera_at(scene, path)?;
    let world = local_to_world(stage, &bridge::prim(stage, path));
    let frame = CameraFrame::read(&cam, world, settings, framing);
    let (camera, refused_meters_per_unit) = lens_camera(&cam, scene, &frame, settings);
    let clip = clipping(&cam);
    let camera = camera.with_clipping(clip.near, clip.far, &clip.planes);
    let read = lens(&cam, &frame);
    Some(RenderCamera {
        camera,
        exposure_scale: exposure_scale(&cam),
        refused_meters_per_unit,
        shutter: offsets(&cam),
        lens: Some(crate::scene::Lens {
            focal_length: read.focal_length,
            aperture: read.aperture,
            aperture_offset: read.aperture_offset,
            conform: framing.policy.as_token().to_owned(),
            pixel_aspect: frame.pixel_aspect,
            f_stop: read.f_stop,
            focus_distance: read.focus_distance,
        }),
        clipping: clip.refusals,
    })
}

/// A camera's `shutter:open` / `shutter:close` as authored: offsets in time
/// codes from the frame, each at USD's fallback 0 when unauthored.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ShutterOffsets {
    pub(super) open: f64,
    pub(super) close: f64,
}

impl ShutterOffsets {
    /// Whether the offsets describe an interval at all: `close > open`. A
    /// non-finite offset describes none.
    pub(super) fn opens(&self) -> bool {
        self.close > self.open
    }
}

fn offsets(cam: &PrimData) -> ShutterOffsets {
    ShutterOffsets {
        open: cam.custom_f64("shutter:open").unwrap_or(0.0),
        close: cam.custom_f64("shutter:close").unwrap_or(0.0),
    }
}

/// The shutter of the camera at `path`, `None` when no camera is there.
pub(super) fn shutter_offsets(scene: &dyn SceneIndex, path: &sdf::Path) -> Option<ShutterOffsets> {
    Some(offsets(&camera_at(scene, path)?))
}

/// The interval motion is measured over, read before the traversal from the
/// camera the render is expected to go through: the frame `f` plus the
/// camera's offsets, `f + open .. f + close`. One interval for the beauty's
/// blur and the `motionvector` AOV, so the two cannot disagree.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Shutter {
    /// The offsets as authored on `camera`.
    pub(super) offsets: ShutterOffsets,
    /// The camera they were read from; `None` when the stage has none the
    /// read could find, which reads as the fallback 0 / 0.
    pub(super) camera: Option<sdf::Path>,
    /// Shutter open and shutter close in time codes; `None` when the shutter
    /// is instantaneous: the offsets open no interval, or there is no frame
    /// to measure from.
    pub(super) interval: Option<(f64, f64)>,
}

impl Shutter {
    pub(super) fn new(
        camera: Option<sdf::Path>,
        offsets: ShutterOffsets,
        frame: Option<f64>,
    ) -> Self {
        let interval = frame
            .filter(|_| offsets.opens())
            .map(|f| (f + offsets.open, f + offsets.close))
            .filter(|(open, close)| close > open);
        Shutter {
            offsets,
            camera,
            interval,
        }
    }

    /// What the MCP session compares to decide whether a settings edit
    /// changed the shutter ([`crate::live::ShutterKey`]); `query` is whether
    /// the import asks prims about time ([`super::xform::TransformMotion`]),
    /// `frame_pose` whether moving placements are posed at the frame.
    pub(super) fn key(&self, query: bool, frame_pose: bool) -> crate::live::ShutterKey {
        crate::live::ShutterKey {
            camera: self.camera.as_ref().map(ToString::to_string),
            offsets: [self.offsets.open.to_bits(), self.offsets.close.to_bits()],
            interval: self.interval.map(|(o, c)| [o.to_bits(), c.to_bits()]),
            query,
            frame_pose,
        }
    }

    /// Offsets that open an interval with no frame to place it at: time
    /// samples are not evaluated over it (`time.motion_needs_frame`).
    pub(super) fn needs_frame(&self) -> bool {
        self.offsets.opens() && self.interval.is_none()
    }
}

/// The linear exposure scale of `cam` at the import's time (C++
/// `UsdGeomCamera::ComputeLinearExposureScale`), computed as C++ computes it,
/// in single precision and in its order:
/// `time × iso × 2^exposure × responsivity / (100 × fStop × fStop)`. Each
/// unauthored attribute reads its schema fallback, which together give
/// exactly 1. `exposure:fStop` is the photometric f-stop; the lens's
/// depth-of-field `fStop` does not enter. Unchecked: a zero `exposure:fStop`
/// gives infinity, which the import refuses once the camera is chosen.
fn exposure_scale(cam: &PrimData) -> f32 {
    let time = cam.custom_f32("exposure:time").unwrap_or(1.0);
    let iso = cam.custom_f32("exposure:iso").unwrap_or(100.0);
    let f_stop = cam.custom_f32("exposure:fStop").unwrap_or(1.0);
    let responsivity = cam.custom_f32("exposure:responsivity").unwrap_or(1.0);
    let exponent = cam.custom_f32("exposure").unwrap_or(0.0);
    (time * iso * 2.0_f32.powf(exponent) * responsivity) / (100.0 * f_stop * f_stop)
}

/// USD's `metersPerUnit` fallback: a centimetre.
const FALLBACK_METERS_PER_UNIT: f64 = 0.01;

/// The stage's authored `metersPerUnit` (C++ `UsdGeomGetStageMetersPerUnit`,
/// which reads only the root and session layers), `None` when no layer
/// authors it: the root's `__usdStageMetadata/metersPerUnit/{value,
/// authored}` (Moho's M3). Unchecked: [`lens_diameter`] refuses a value that
/// is not a length.
fn authored_meters_per_unit(scene: &dyn SceneIndex) -> Option<f64> {
    match stage_metadata(scene, "metersPerUnit")? {
        (sdf::Value::Double(m), true) => Some(m),
        _ => None,
    }
}

/// The lens diameter in scene units: `focalLength / fStop` millimetres,
/// converted through the stage's `metersPerUnit` (unauthored reads as
/// [`FALLBACK_METERS_PER_UNIT`]), and the unit when it was refused. `0` is a
/// pinhole: an `fStop` of 0 or less, or `disableDepthOfField`. Only a lens
/// reads the unit, so a pinhole render refuses none it does not use; a unit
/// that is not finite, not positive, or so small that the diameter overflows
/// `f32` is refused and reads as the fallback.
fn lens_diameter(
    scene: &dyn SceneIndex,
    focal_length: f32,
    f_stop: f32,
    depth_of_field: bool,
) -> (f32, Option<f64>) {
    if !depth_of_field || f_stop <= 0.0 {
        return (0.0, None);
    }
    let diameter = |meters_per_unit: f64| {
        (f64::from(focal_length) / f64::from(f_stop) * 0.001 / meters_per_unit) as f32
    };
    match authored_meters_per_unit(scene) {
        // A unit so small that the diameter overflows `f32` is no length
        // either: the lens would offset every ray to infinity.
        Some(m) if m.is_finite() && m > 0.0 && diameter(m).is_finite() => (diameter(m), None),
        Some(m) => (diameter(FALLBACK_METERS_PER_UNIT), Some(m)),
        None => (diameter(FALLBACK_METERS_PER_UNIT), None),
    }
}

/// The ray-generating camera of `frame`, from its transform and lens, and
/// the stage unit its lens refused.
fn lens_camera(
    cam: &PrimData,
    scene: &dyn SceneIndex,
    frame: &CameraFrame,
    settings: &RenderSettings,
) -> (Camera, Option<f64>) {
    let CameraLens {
        f_stop,
        focus_distance,
        ..
    } = lens(cam, frame);

    if frame.orthographic {
        // No lens: the window is the apertures, and depth of field does not
        // apply. The offsets move the window as they move a perspective one:
        // the shift in frames is the same, the window and the offsets both
        // being in tenths of a unit.
        let [view_width, view_height] = frame.ortho_window();
        let lookfrom = Vec3A::from(frame.eye);
        let lookat = Vec3A::from(frame.eye + frame.forward);
        let vup = Vec3A::from(frame.up);
        debug!(
            "USD camera: orthographic lookfrom={:?} lookat={:?} vup={:?} window={}x{} (scene \
             units)",
            lookfrom, lookat, vup, view_width, view_height
        );
        return (
            Camera::orthographic(lookfrom, lookat, vup, view_width, view_height)
                .with_window_shift(frame.window_shift()),
            None,
        );
    }

    let vfov_deg = 2.0 * frame.tan_half_vfov().atan().to_degrees();
    let (aperture, refused_meters_per_unit) =
        lens_diameter(scene, frame.focal_length, f_stop, settings.depth_of_field());
    // A pinhole has no focus plane, and USD's `focusDistance = 0` (what
    // GfCamera writes, and a DCC camera with focus off) would put the image
    // plane on the eye: every ray would have no direction. A pinhole's rays
    // do not depend on the plane's distance, so it goes at unit distance; a
    // positive, finite focus keeps its own, bit for bit.
    let focus_distance = if aperture == 0.0 && !(focus_distance > 0.0 && focus_distance.is_finite())
    {
        1.0
    } else {
        focus_distance
    };
    let aspect = frame.aspect();
    let lookfrom = Vec3A::from(frame.eye);
    let lookat = Vec3A::from(frame.eye + frame.forward * focus_distance);
    let vup = Vec3A::from(frame.up);

    debug!(
        "USD camera: lookfrom={:?} lookat={:?} vup={:?} vfov={} aspect={} lens diameter={} \
         (scene units) focus={}",
        lookfrom, lookat, vup, vfov_deg, aspect, aperture, focus_distance
    );

    let camera = Camera::new(
        lookfrom,
        lookat,
        vup,
        vfov_deg,
        aspect,
        aperture,
        focus_distance,
    )
    .with_window_shift(frame.window_shift());
    (camera, refused_meters_per_unit)
}

/// What adaptive subdivision needs of the render camera, read before the
/// traversal builds it: the position and the pixels per world unit at unit
/// distance, `image height / (2 tan(vfov / 2))` = `height · focal / aperture`,
/// the aperture conformed. The same [`CameraFrame`] as [`build_camera`]'s.
/// `None` when `prim` is not a camera on `stage`.
pub(super) fn screen_projection(
    scene: &dyn SceneIndex,
    stage: &Stage,
    path: &sdf::Path,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<ScreenProjection> {
    let cam = camera_at(scene, path)?;
    let world = local_to_world(stage, &bridge::prim(stage, path));
    let frame = CameraFrame::read(&cam, world, settings, framing);
    if frame.orthographic {
        // Pixels per world unit at every distance, and the window's half
        // extents and centre in place of the tangents. A shifted window views
        // every placement at the same rate: there is no off-axis gain.
        let [view_width, view_height] = frame.ortho_window();
        let f_px = frame.height / view_height * narrow_pixel_gain(frame.pixel_aspect);
        return (f_px.is_finite() && f_px > 0.0).then_some(ScreenProjection {
            eye: frame.eye,
            f_px,
            orthographic: true,
            forward: frame.forward,
            up: frame.up,
            tan_v: view_height / 2.0,
            tan_h: view_width / 2.0,
            centre: frame.ortho_centre(),
        });
    }
    let f_px = frame.height * frame.focal_length / frame.vert_aperture();
    // The view pyramid, as `build_camera` builds it: the vertical field of
    // view from the conformed lens, the horizontal one from the window's
    // aspect ratio — which the conformed aperture has. Square pixels
    // multiply by exactly 1.
    let tan_v = frame.tan_half_vfov();
    let tan_h = tan_v * frame.width / frame.height * frame.pixel_aspect;
    // The offsets move the window, so the pyramid leans as the rays do:
    // its centre is the shift in frames of `2 tan`.
    let [shift_h, shift_v] = frame.window_shift();
    let centre = [shift_h * 2.0 * tan_h, shift_v * 2.0 * tan_v];
    let f_px = f_px * off_axis_gain(tan_h, tan_v, centre) * narrow_pixel_gain(frame.pixel_aspect);
    (f_px.is_finite() && f_px > 0.0).then_some(ScreenProjection {
        eye: frame.eye,
        f_px,
        orthographic: false,
        forward: frame.forward,
        up: frame.up,
        tan_v,
        tan_h,
        centre,
    })
}

/// How much finer than the vertical rate a pixel aspect makes the
/// horizontal one, as a factor on the pixel rate: a pixel `p` times as wide
/// as it is tall spans `1 / p` of a vertical rate's pixel across. Pixels
/// wider than tall leave the vertical rate the finer one, which the rate
/// already is (gain 1); narrower ones raise it, so the rate errs toward
/// detail on both axes. Square pixels gain exactly 1.
fn narrow_pixel_gain(pixel_aspect: f32) -> f32 {
    if pixel_aspect < 1.0 {
        1.0 / pixel_aspect
    } else {
        1.0
    }
}

/// How much more a shifted window can undercount than the same window
/// centred, as a factor on the pixel rate.
///
/// The rate is `f_px / d`, `d` the distance to the eye, but a small edge at
/// tangents `(x, y)` off the view direction spans up to `f_px · d / z²`
/// pixels, `z` its depth: `(d / z)² = 1 + x² + y²` times the estimate. Within
/// a centred window that is at most `1 + tan_h² + tan_v²` at a corner — a
/// known gap of the rate, not closed here. A shift reaches further corners,
/// `(|c_h| + tan_h, |c_v| + tan_v)`; scaling by the ratio of the two bounds
/// keeps a shifted frame's worst undercount at the centred one's. A centred
/// window gains exactly 1, so its rate is unchanged bit for bit.
fn off_axis_gain(tan_h: f32, tan_v: f32, centre: [f32; 2]) -> f32 {
    if centre == [0.0, 0.0] {
        return 1.0;
    }
    let reach = |c: f32, t: f32| (c.abs() + t) * (c.abs() + t);
    let shifted = 1.0 + reach(centre[0], tan_h) + reach(centre[1], tan_v);
    let centred = 1.0 + tan_h * tan_h + tan_v * tan_v;
    let gain = shifted / centred;
    if gain.is_finite() { gain.max(1.0) } else { 1.0 }
}

/// The render camera as adaptive subdivision needs it, read before traversal.
pub(super) struct ScreenProjection {
    pub(super) eye: Vec3,
    /// Pixels per world unit at unit distance, raised for a shifted window
    /// by [`off_axis_gain`]; at every distance when `orthographic`.
    pub(super) f_px: f32,
    /// Whether the camera's rays are parallel.
    pub(super) orthographic: bool,
    /// The view direction and the image's up, in world space.
    pub(super) forward: Vec3,
    pub(super) up: Vec3,
    /// Tangents of the half fields of view, vertical and horizontal; when
    /// `orthographic`, the half height and width of the window, in world
    /// units.
    pub(super) tan_v: f32,
    pub(super) tan_h: f32,
    /// The tangents of the window's centre, horizontal and vertical: off the
    /// view direction when the aperture is offset. When `orthographic`, the
    /// centre's offset from the view axis, in world units.
    pub(super) centre: [f32; 2],
}

/// Composed local-to-world by walking the prim path upwards. Slower than
/// tracking it during DFS, but exact and only used at build_camera time.
pub(super) fn local_to_world(stage: &Stage, prim: &Prim) -> GMat4 {
    let mut ancestors: Vec<Prim> = Vec::new();
    let mut cur_path = prim.path().clone();
    ancestors.push(prim_at(stage, cur_path.clone()));
    while let Some(parent) = cur_path.parent() {
        cur_path = parent;
        ancestors.push(prim_at(stage, cur_path.clone()));
        if cur_path.as_str() == "/" {
            break;
        }
    }
    ancestors.reverse();
    ancestors
        .iter()
        .fold(GMat4::IDENTITY, |acc, p| compose_with_parent(p, acc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::settings::CameraChoice;
    use crate::scene::usd_import::{read_camera_ahead, stage_builder};
    use openusd::usd::InitialLoadSet;

    // The readers take a scene index; these build one over the test's stage,
    // so each test reads its camera as the import does.
    fn scene_of(stage: &Stage) -> crate::scene::usd_import::index::Input {
        crate::scene::usd_import::index::input_chain(stage.clone(), None, false)
    }

    fn build_camera(
        stage: &Stage,
        prim: &Prim,
        settings: &RenderSettings,
        framing: Framing,
    ) -> Option<RenderCamera> {
        let scene = scene_of(stage);
        super::build_camera(scene.chain.as_ref(), stage, prim.path(), settings, framing)
    }

    fn camera_lens(
        stage: &Stage,
        prim: &Prim,
        settings: &RenderSettings,
        framing: Framing,
    ) -> Option<CameraLens> {
        let scene = scene_of(stage);
        super::camera_lens(scene.chain.as_ref(), stage, prim.path(), settings, framing)
    }

    fn screen_projection(
        stage: &Stage,
        prim: &Prim,
        settings: &RenderSettings,
        framing: Framing,
    ) -> Option<ScreenProjection> {
        let scene = scene_of(stage);
        super::screen_projection(scene.chain.as_ref(), stage, prim.path(), settings, framing)
    }

    fn authored_meters_per_unit(stage: &Stage) -> Option<f64> {
        super::authored_meters_per_unit(scene_of(stage).chain.as_ref())
    }

    /// A stage on a `.usda` holding `metadata` and nothing else.
    fn stage(name: &str, metadata: &str) -> Stage {
        let dir = std::env::temp_dir().join("crust_camera_units");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, format!("#usda 1.0\n(\n{metadata}\n)\n")).expect("write stage");
        super::super::stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens")
    }

    /// The authored unit, and `None` (not the schema's fallback) when no
    /// layer authors one.
    #[test]
    fn meters_per_unit_reads_what_the_stage_authors() {
        let metre = stage("metre", "    metersPerUnit = 1");
        assert_eq!(authored_meters_per_unit(&metre), Some(1.0));
        let unauthored = stage("unauthored", "    upAxis = \"Y\"");
        assert_eq!(authored_meters_per_unit(&unauthored), None);
    }

    fn write(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("crust_shutter_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, text).expect("write stage");
        path
    }

    /// The shutter read ahead of the traversal off `stage`'s index (payloads
    /// unloaded), through the camera at `named` when one is.
    fn shutter(name: &str, stage: &str, named: Option<&str>, frame: Option<f64>) -> Shutter {
        let path = write(name, stage);
        let path_str = path.to_str().unwrap();
        let index = stage_builder()
            .load(InitialLoadSet::LoadNone)
            .open(path_str)
            .expect("stage opens");
        let named = named.map(|p| CameraChoice::Requested(sdf::path(p).unwrap()));
        let scene = crate::scene::usd_import::index::input_chain(index.clone(), frame, true);
        read_camera_ahead(
            &index,
            scene.chain.as_ref(),
            &path,
            path_str,
            named.as_ref(),
            None,
            &RenderSettings::default(),
            Framing::default(),
            frame,
        )
        .expect("reads")
        .1
    }

    /// Every policy over a 36 × 24 aperture at 1:1, 2:1 and 1:2, and at 2:1
    /// of pixels twice as wide as tall, against
    /// apertures worked out by hand from the USD documentation's table, not
    /// by calling `conform_aperture`.
    #[test]
    fn the_five_policies_conform_a_36_by_24_aperture() {
        use AspectRatioConformPolicy::*;
        let cases = [
            ((100, 100), ExpandAperture, [36.0, 36.0], 1.0),
            ((100, 100), CropAperture, [24.0, 24.0], 1.0),
            ((100, 100), AdjustApertureWidth, [24.0, 24.0], 1.0),
            ((100, 100), AdjustApertureHeight, [36.0, 36.0], 1.0),
            ((100, 100), AdjustPixelAspectRatio, [36.0, 24.0], 1.5),
            ((200, 100), ExpandAperture, [48.0, 24.0], 1.0),
            ((200, 100), CropAperture, [36.0, 18.0], 1.0),
            ((200, 100), AdjustApertureWidth, [48.0, 24.0], 1.0),
            ((200, 100), AdjustApertureHeight, [36.0, 18.0], 1.0),
            ((200, 100), AdjustPixelAspectRatio, [36.0, 24.0], 0.75),
            ((100, 200), ExpandAperture, [36.0, 72.0], 1.0),
            ((100, 200), CropAperture, [12.0, 24.0], 1.0),
            ((100, 200), AdjustApertureWidth, [12.0, 24.0], 1.0),
            ((100, 200), AdjustApertureHeight, [36.0, 72.0], 1.0),
            ((100, 200), AdjustPixelAspectRatio, [36.0, 24.0], 3.0),
            // Pixels twice as wide as tall: a 200 × 100 image is 4:1.
            ((200, 100), ExpandAperture, [96.0, 24.0], 2.0),
            ((200, 100), CropAperture, [36.0, 9.0], 2.0),
            ((200, 100), AdjustApertureWidth, [96.0, 24.0], 2.0),
            ((200, 100), AdjustApertureHeight, [36.0, 9.0], 2.0),
            // The authored pixel aspect gives way to the aperture's.
            ((200, 100), AdjustPixelAspectRatio, [36.0, 24.0], 0.75),
        ];
        for (i, (resolution, policy, want, pixel_aspect)) in cases.into_iter().enumerate() {
            // The last five author pixels twice as wide as tall.
            let authored = if i < 15 { 1.0 } else { 2.0 };
            assert_eq!(
                conform(policy, resolution, authored, [36.0, 24.0]),
                (want, pixel_aspect),
                "{policy:?} at {resolution:?}, pixelAspectRatio {authored}"
            );
        }
    }

    /// A pixel aspect `adjustPixelAspectRatio` would derive that no EXR
    /// header can hold frames as `expandAperture` with square pixels, so the
    /// render's files can still be written; so does a degenerate aperture.
    #[test]
    fn a_derived_pixel_aspect_out_of_range_frames_as_expand_aperture() {
        use AspectRatioConformPolicy::*;
        let sliver = [3.6e6, 1.0];
        assert_eq!(
            conform(AdjustPixelAspectRatio, (100, 100), 1.0, sliver),
            (conform(ExpandAperture, (100, 100), 1.0, sliver).0, 1.0)
        );
        assert_eq!(
            conform(AdjustPixelAspectRatio, (100, 100), 1.0, [0.0, 24.0]),
            ([0.0, 24.0], 1.0)
        );
    }

    /// An aperture that already has the image's shape is left alone, bit
    /// for bit, by every policy, and keeps the authored pixel shape.
    #[test]
    fn a_matching_aperture_is_not_conformed() {
        use AspectRatioConformPolicy::*;
        // 0.1 / (0.1 · 3 / 4) is not exactly 4 / 3 in f32.
        let near = [0.1_f32, 0.1 * 3.0 / 4.0];
        for policy in [
            ExpandAperture,
            CropAperture,
            AdjustApertureWidth,
            AdjustApertureHeight,
            AdjustPixelAspectRatio,
        ] {
            assert_eq!(
                conform(policy, (200, 100), 1.0, [36.0, 18.0]),
                ([36.0, 18.0], 1.0)
            );
            assert_eq!(conform(policy, (400, 300), 1.0, near), (near, 1.0));
            // 36 × 9 is 200 × 100 of pixels twice as wide as tall.
            assert_eq!(
                conform(policy, (200, 100), 2.0, [36.0, 9.0]),
                ([36.0, 9.0], 2.0)
            );
        }
    }

    /// A stage with one camera `/Cam` authoring `aperture`.
    fn camera_stage(name: &str, aperture: &str) -> (Stage, Prim) {
        let path = write(
            name,
            &format!("#usda 1.0\ndef Camera \"Cam\"\n{{\n{aperture}\n}}\n"),
        );
        let stage = stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let prim = prim_at(&stage, sdf::path("/Cam").unwrap());
        (stage, prim)
    }

    const APERTURE_36_24: &str =
        "    float horizontalAperture = 36\n    float verticalAperture = 24";

    fn framed(policy: AspectRatioConformPolicy) -> Framing {
        Framing {
            policy,
            ..Framing::default()
        }
    }

    /// The tangents of the camera-space ray through `(s, t)`: right over
    /// depth, up over depth.
    fn tangents(camera: &Camera, s: f32, t: f32) -> [f32; 2] {
        let d = camera.get_ray(s, t, [0.5, 0.5], 0.0).direction();
        [d.x / -d.z, d.y / -d.z]
    }

    /// `adjustPixelAspectRatio` keeps the aperture exactly and renders the
    /// pixels it needs: 36 × 24 over 200 × 100 is pixels 0.75 as wide as
    /// tall, the frame's edges are the aperture's, and nothing is warned.
    #[test]
    fn adjust_pixel_aspect_ratio_keeps_the_aperture() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("pixel_aspect", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(200, 100);
        let scope = crate::WarningScope::enter();
        let par = build_camera(&stage, &prim, &settings, framed(AdjustPixelAspectRatio))
            .expect("a camera");
        assert!(scope.finish().is_empty());
        let lens = par.lens.expect("a lens");
        assert_eq!(lens.pixel_aspect, 0.75);
        // The default 50 mm lens: the right edge is 18 / 50 off the axis,
        // the top 12 / 50.
        let [right, _] = tangents(&par.camera, 1.0, 0.5);
        let [_, top] = tangents(&par.camera, 0.5, 1.0);
        assert!((right - 0.36).abs() < 1e-5, "{right}");
        assert!((top - 0.24).abs() < 1e-5, "{top}");
        let lens = camera_lens(&stage, &prim, &settings, framed(AdjustPixelAspectRatio)).unwrap();
        assert_eq!(lens.conformed_aperture, [36.0, 24.0]);

        // An aperture that already has the image's aspect keeps square pixels.
        let (stage, prim) = camera_stage(
            "pixel_aspect_matching",
            "    float horizontalAperture = 36\n    float verticalAperture = 18",
        );
        let par = build_camera(&stage, &prim, &settings, framed(AdjustPixelAspectRatio))
            .expect("a camera");
        assert_eq!(par.lens.expect("a lens").pixel_aspect, 1.0);
    }

    /// An authored pixel aspect is conformed to: pixels twice as wide as
    /// tall make a 200 × 100 image a 4:1 window, which `expandAperture`
    /// reaches by widening a 36 × 24 aperture to 96 × 24. The rays and the
    /// projection dicing reads both span it.
    #[test]
    fn an_authored_pixel_aspect_widens_the_window() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("pixel_aspect_authored", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(200, 100);
        let framing = Framing {
            policy: ExpandAperture,
            pixel_aspect: 2.0,
        };
        let rc = build_camera(&stage, &prim, &settings, framing).expect("a camera");
        assert_eq!(rc.lens.expect("a lens").pixel_aspect, 2.0);
        let [right, _] = tangents(&rc.camera, 1.0, 0.5);
        let [_, top] = tangents(&rc.camera, 0.5, 1.0);
        assert!((right - 0.96).abs() < 1e-5, "{right}");
        assert!((top - 0.24).abs() < 1e-5, "{top}");
        let p = screen_projection(&stage, &prim, &settings, framing).unwrap();
        assert!((p.tan_h - 0.96).abs() < 1e-6 && (p.tan_v - 0.24).abs() < 1e-6);
        // Wide pixels leave the vertical rate the finer one.
        assert_eq!(p.f_px, 100.0 * 50.0 / 24.0);
        // Narrow pixels raise it to the horizontal one: 0.5 of a pixel's
        // height across is twice the vertical rate.
        let narrow = Framing {
            policy: ExpandAperture,
            pixel_aspect: 0.5,
        };
        let p = screen_projection(&stage, &prim, &settings, narrow).unwrap();
        // 200 × 100 of half-width pixels is 1:1: the aperture grows to 36 × 36.
        assert!(
            (p.f_px - 2.0 * 100.0 * 50.0 / 36.0).abs() < 1e-3,
            "{}",
            p.f_px
        );
    }

    /// Square pixels authored as `pixelAspectRatio = 1` build the camera an
    /// unauthored one does, bit for bit.
    #[test]
    fn square_pixels_are_the_unauthored_camera() {
        let (stage, prim) = camera_stage("pixel_aspect_square", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(300, 100);
        let a = build_camera(&stage, &prim, &settings, Framing::default()).unwrap();
        let b = build_camera(
            &stage,
            &prim,
            &settings,
            Framing {
                pixel_aspect: 1.0,
                ..Framing::default()
            },
        )
        .unwrap();
        assert_eq!(format!("{:?}", a.camera), format!("{:?}", b.camera));
    }

    /// Every reader sees the conformed aperture: the lens `ls` reports, the
    /// projection adaptive subdivision reads and the rays agree.
    #[test]
    fn every_reader_reads_the_conformed_aperture() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("readers", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(200, 100);
        let lens = camera_lens(&stage, &prim, &settings, framed(AdjustApertureHeight)).unwrap();
        assert_eq!(lens.aperture, [36.0, 24.0]);
        assert_eq!(lens.conformed_aperture, [36.0, 18.0]);
        let height =
            screen_projection(&stage, &prim, &settings, framed(AdjustApertureHeight)).unwrap();
        let expand = screen_projection(&stage, &prim, &settings, framed(ExpandAperture)).unwrap();
        // 18 mm of film over 100 pixels against 24 mm.
        assert_eq!(height.f_px, 100.0 * 50.0 / 18.0);
        assert_eq!(expand.f_px, 100.0 * 50.0 / 24.0);
        assert_eq!(height.tan_v, 18.0 / 100.0);
        assert_eq!(height.tan_h, height.tan_v * 2.0);
    }

    /// An unauthored vertical aperture is the schema's 15.2908 and is
    /// conformed like an authored one (#388): on an image wider than
    /// 20.955 × 15.2908, `expandAperture` widens the horizontal aperture, as
    /// usdview and Storm frame it; on a narrower one it grows the vertical
    /// aperture to the horizontal one over the image's aspect.
    #[test]
    fn an_unauthored_vertical_aperture_is_the_schema_fallback_conformed() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("unauthored_apertures", "    float focalLength = 50");
        let wide = RenderSettings::default().with_resolution(1920, 1080);
        let lens = camera_lens(&stage, &prim, &wide, framed(ExpandAperture)).unwrap();
        assert_eq!(lens.aperture, [20.955, FALLBACK_VERTICAL_APERTURE]);
        let [h, v] = lens.conformed_aperture;
        assert_eq!(v, FALLBACK_VERTICAL_APERTURE);
        assert!(
            (h - FALLBACK_VERTICAL_APERTURE * 16.0 / 9.0).abs() < 1e-4,
            "{h}"
        );
        let crop = camera_lens(&stage, &prim, &wide, framed(CropAperture)).unwrap();
        let [h, v] = crop.conformed_aperture;
        assert_eq!(h, 20.955);
        assert!((v - 20.955 * 9.0 / 16.0).abs() < 1e-4, "{v}");
        let square = RenderSettings::default().with_resolution(100, 100);
        let lens = camera_lens(&stage, &prim, &square, framed(ExpandAperture)).unwrap();
        assert_eq!(lens.conformed_aperture, [20.955, 20.955]);
        // `adjustPixelAspectRatio` now has a mismatched aperture to answer
        // for, as it would in USD: the fallback's 1.370 over 16:9 pixels.
        let par = build_camera(&stage, &prim, &wide, framed(AdjustPixelAspectRatio)).unwrap();
        let want = 20.955 / FALLBACK_VERTICAL_APERTURE / (1920.0 / 1080.0);
        let got = par.lens.expect("a lens").pixel_aspect;
        assert!((got - want).abs() < 1e-6, "{got}");
    }

    /// An orthographic camera's window is its conformed apertures in tenths
    /// of a scene unit, for the rays and for the projection dicing reads
    /// alike; the focal length plays no part.
    #[test]
    fn an_orthographic_window_is_the_conformed_apertures_in_tenths() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage(
            "orthographic",
            "    token projection = \"orthographic\"\n    float focalLength = 12\n\
             \x20   float horizontalAperture = 36\n    float verticalAperture = 24",
        );
        let settings = RenderSettings::default().with_resolution(200, 100);
        let p = screen_projection(&stage, &prim, &settings, framed(AdjustApertureHeight)).unwrap();
        assert!(p.orthographic);
        // A 3.6 × 1.8 window over 100 pixels of height.
        assert!((p.f_px - 100.0 / 1.8).abs() < 1e-4, "{}", p.f_px);
        assert!((p.tan_h - 1.8).abs() < 1e-6 && (p.tan_v - 0.9).abs() < 1e-6);
        let camera = build_camera(&stage, &prim, &settings, framed(AdjustApertureHeight))
            .unwrap()
            .camera;
        assert!(camera.is_orthographic());
        let corner = camera.get_ray(1.0, 1.0, [0.5, 0.5], 0.0).origin();
        assert!(
            corner.abs_diff_eq(Vec3A::new(1.8, 0.9, 0.0), 1e-6),
            "{corner}"
        );
    }

    /// An orthographic camera's offsets move its window by the offsets in
    /// tenths of a scene unit, as they move a perspective one's film back,
    /// and the dicing box moves with it.
    #[test]
    fn an_orthographic_window_moves_with_its_offsets() {
        let (stage, prim) = camera_stage(
            "orthographic_offset",
            "    token projection = \"orthographic\"\n    float horizontalAperture = 40\n\
             \x20   float verticalAperture = 20\n    float horizontalApertureOffset = -10\n\
             \x20   float verticalApertureOffset = 5",
        );
        let settings = RenderSettings::default().with_resolution(200, 100);
        let camera = build_camera(&stage, &prim, &settings, Framing::default())
            .unwrap()
            .camera;
        // A 4 × 2 window centred at (−1, 0.5).
        for (s, t, x, y) in [
            (0.5, 0.5, -1.0, 0.5),
            (0.0, 0.0, -3.0, -0.5),
            (1.0, 1.0, 1.0, 1.5),
        ] {
            let origin = camera.get_ray(s, t, [0.5, 0.5], 0.0).origin();
            assert!(
                origin.abs_diff_eq(Vec3A::new(x, y, 0.0), 1e-5),
                "({s}, {t}): {origin}"
            );
        }
        // The projection inverts the shifted window.
        let (s, t) = camera.frame(200, 100).proj(Vec3A::new(-1.0, 0.5, -7.0));
        assert!((s - 0.5).abs() < 1e-5 && (t - 0.5).abs() < 1e-5, "{s} {t}");
        let p = screen_projection(&stage, &prim, &settings, Framing::default()).unwrap();
        assert!(p.orthographic);
        assert!((p.centre[0] + 1.0).abs() < 1e-6 && (p.centre[1] - 0.5).abs() < 1e-6);
        // No off-axis gain: the rate is the window's alone.
        assert!((p.f_px - 50.0).abs() < 1e-3, "{}", p.f_px);
    }

    /// The direction of the pinhole ray through `(s, t)`, in the camera's
    /// own space (`/Cam` is unplaced: +X right, +Y up, looking down −Z),
    /// scaled so that it lands at `z = −focal`.
    fn film_point(camera: &Camera, s: f32, t: f32, focal: f32) -> Vec3A {
        let d = camera.get_ray(s, t, [0.0, 0.0], 0.0).direction();
        d * (focal / -d.z)
    }

    fn assert_near(got: Vec3A, want: Vec3A) {
        assert!(
            (got - want).abs().max_element() <= 1e-4 * want.abs().max_element(),
            "{got:?} is not {want:?}"
        );
    }

    /// The offsets move the window off the axis, in the apertures' units:
    /// the ray through `(s, t)` points along
    /// `((s − ½)·H_ap + H_off, (t − ½)·V_ap + V_off, −focalLength)`.
    #[test]
    fn aperture_offsets_shift_the_window() {
        let (stage, prim) = camera_stage(
            "aperture_offset",
            "    float focalLength = 35\n    float horizontalAperture = 36\n    \
             float verticalAperture = 24\n    float horizontalApertureOffset = -3\n    \
             float verticalApertureOffset = 6",
        );
        let settings = RenderSettings::default().with_resolution(300, 200);
        let camera = build_camera(&stage, &prim, &settings, Framing::default())
            .expect("a camera")
            .camera;
        for (s, t) in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (0.25, 0.75)] {
            let want = Vec3A::new((s - 0.5) * 36.0 - 3.0, (t - 0.5) * 24.0 + 6.0, -35.0);
            assert_near(film_point(&camera, s, t, 35.0), want);
        }
        // The motion vector's projection inverts the shifted rays.
        let frame = camera.frame(300, 200);
        let (s, t) = frame.proj(Vec3A::new(-3.0, 6.0, -35.0) * 4.0);
        assert!((s - 0.5).abs() < 1e-5 && (t - 0.5).abs() < 1e-5, "{s} {t}");
    }

    /// The conform resizes the window about its centre, as OpenUSD's
    /// `CameraUtilConformWindow` does: the offset is not scaled with it.
    #[test]
    fn the_conform_keeps_the_offset() {
        let (stage, prim) = camera_stage(
            "aperture_offset_conformed",
            "    float focalLength = 35\n    float horizontalAperture = 36\n    \
             float verticalAperture = 24\n    float verticalApertureOffset = 3",
        );
        let settings = RenderSettings::default().with_resolution(200, 100);
        let framing = framed(AspectRatioConformPolicy::AdjustApertureHeight);
        let camera = build_camera(&stage, &prim, &settings, framing)
            .expect("a camera")
            .camera;
        // 36 × 18 conformed, centred 3 mm up.
        assert_near(
            film_point(&camera, 0.5, 0.5, 35.0),
            Vec3A::new(0.0, 3.0, -35.0),
        );
        assert_near(
            film_point(&camera, 1.0, 1.0, 35.0),
            Vec3A::new(18.0, 12.0, -35.0),
        );
        // The dicing pyramid leans with the window: centre 3 / 35 up, the
        // same half height of 9 / 35.
        let p = screen_projection(&stage, &prim, &settings, framing).unwrap();
        assert!((p.centre[1] - 3.0 / 35.0).abs() < 1e-6, "{:?}", p.centre);
        assert_eq!(p.centre[0], 0.0);
        assert!((p.tan_v - 9.0 / 35.0).abs() < 1e-6);
    }

    /// The largest pixel span of a short edge at `p` (camera space, the
    /// camera unplaced), over edge directions: the true projected size the
    /// screen rate estimates.
    fn projected_px(camera: &Camera, (w, h): (usize, usize), p: Vec3A, len: f32) -> f32 {
        let frame = camera.frame(w, h);
        let (s0, t0) = frame.proj(p);
        let mut most = 0.0_f32;
        for i in 0..16 {
            for j in 0..8 {
                let (a, b) = (i as f32 * std::f32::consts::TAU / 16.0, j as f32 * 0.4);
                let e = Vec3A::new(a.cos() * b.cos(), a.sin() * b.cos(), b.sin());
                let (s1, t1) = frame.proj(p + e * len);
                let px = ((s1 - s0) * w as f32).hypot((t1 - t0) * h as f32);
                most = most.max(px);
            }
        }
        most
    }

    /// At the frame's far corner, the screen rate undercounts a shifted
    /// camera's edge no more than the centred camera's at its own corner
    /// (Qodo on #399: the eye-distance rate undercounts off axis by
    /// `1 + tan²`, and a shift reaches larger tangents).
    #[test]
    fn a_shifted_window_is_rated_no_coarser_than_a_centred_one() {
        let settings = RenderSettings::default().with_resolution(300, 200);
        let undercount = |name, offset: &str| {
            let (stage, prim) = camera_stage(
                name,
                &format!(
                    "    float focalLength = 20\n    float horizontalAperture = 36\n    \
                     float verticalAperture = 24\n{offset}"
                ),
            );
            let camera = build_camera(&stage, &prim, &settings, Framing::default())
                .unwrap()
                .camera;
            let rate = screen_projection(&stage, &prim, &settings, Framing::default()).unwrap();
            // Just inside the corner furthest from the view direction.
            let d = camera.get_ray(0.999, 0.999, [0.0, 0.0], 0.0).direction();
            let p = d * (10.0 / -d.z);
            let len = 1e-3;
            let estimate = rate.f_px * len / p.length();
            estimate / projected_px(&camera, (300, 200), p, len)
        };
        let centred = undercount("rate_centred", "");
        let shifted = undercount(
            "rate_shifted",
            "    float horizontalApertureOffset = 30\n    float verticalApertureOffset = 20",
        );
        assert!(
            centred < 1.0,
            "the centred corner is undercounted: {centred}"
        );
        assert!(
            shifted >= centred * 0.99,
            "shifted undercount {shifted} is worse than the centred {centred}"
        );
    }

    /// Unauthored and zero offsets build the centred camera, bit for bit.
    #[test]
    fn a_zero_offset_is_the_centred_camera() {
        let settings = RenderSettings::default().with_resolution(300, 200);
        let build = |name, text| {
            let (stage, prim) = camera_stage(name, text);
            let c = build_camera(&stage, &prim, &settings, Framing::default()).unwrap();
            format!("{:?}", c.camera)
        };
        assert_eq!(
            build("offset_unauthored", APERTURE_36_24),
            build(
                "offset_zero",
                "    float horizontalAperture = 36\n    float verticalAperture = 24\n    \
                 float horizontalApertureOffset = 0\n    float verticalApertureOffset = 0"
            )
        );
    }

    fn one_camera(shutter: &str) -> String {
        format!(
            "#usda 1.0\ndef Xform \"World\"\n{{\n    def Camera \"Cam\"\n    {{\n{shutter}\n    }}\n}}\n"
        )
    }

    #[test]
    fn an_authored_shutter_is_measured_from_the_frame() {
        let s = shutter(
            "open_close",
            &one_camera("        double shutter:open = 0\n        double shutter:close = 0.5"),
            None,
            Some(5.0),
        );
        assert_eq!(s.interval, Some((5.0, 5.5)));
        assert_eq!(s.camera, Some(sdf::path("/World/Cam").unwrap()));
    }

    #[test]
    fn a_shutter_may_open_before_the_frame() {
        let s = shutter(
            "centred",
            &one_camera("        double shutter:open = -0.25\n        double shutter:close = 0.25"),
            None,
            Some(5.0),
        );
        assert_eq!(s.interval, Some((4.75, 5.25)));
    }

    #[test]
    fn an_unauthored_shutter_is_instantaneous() {
        let s = shutter("unauthored", &one_camera(""), None, Some(5.0));
        assert_eq!(s.offsets, ShutterOffsets::default());
        assert_eq!(s.interval, None);
        assert!(!s.needs_frame());
    }

    #[test]
    fn without_a_frame_the_shutter_is_instantaneous() {
        let s = shutter(
            "no_frame",
            &one_camera("        double shutter:close = 0.5"),
            None,
            None,
        );
        assert_eq!(s.interval, None);
        assert!(s.needs_frame());
    }

    #[test]
    fn a_named_camera_wins_over_an_earlier_one() {
        let stage = "#usda 1.0\ndef Xform \"World\"\n{\n    def Camera \"A\"\n    {\n        \
                     double shutter:close = 1\n    }\n    def Camera \"B\"\n    {\n        \
                     double shutter:close = 0.5\n    }\n}\n";
        let s = shutter("named", stage, Some("/World/A"), Some(2.0));
        assert_eq!(s.interval, Some((2.0, 3.0)));
        // Unnamed, the walk meets B first, as the traversal does (last child
        // first).
        let s = shutter("unnamed", stage, None, Some(2.0));
        assert_eq!(s.camera, Some(sdf::path("/World/B").unwrap()));
        assert_eq!(s.interval, Some((2.0, 2.5)));
    }

    #[test]
    fn a_named_camera_in_an_unloaded_payload_is_read_through_a_masked_stage() {
        write(
            "payload_camera_shot",
            "#usda 1.0\ndef Xform \"Shot\"\n{\n    def Camera \"Cam\"\n    {\n        \
             double shutter:open = -0.5\n        double shutter:close = 0.5\n    }\n}\n",
        );
        let stage = "#usda 1.0\ndef Xform \"World\"\n{\n    def Xform \"Shot\" (\n        \
                     prepend payload = @payload_camera_shot.usda@</Shot>\n    )\n    {\n    }\n}\n";
        let s = shutter("payload_camera", stage, Some("/World/Shot/Cam"), Some(10.0));
        assert_eq!(s.interval, Some((9.5, 10.5)));
        // Unnamed, the index walk cannot see it.
        let s = shutter("payload_camera_unnamed", stage, None, Some(10.0));
        assert_eq!((s.camera, s.interval), (None, None));
    }
}
