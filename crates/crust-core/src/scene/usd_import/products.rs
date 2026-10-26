//! `RenderSettings.products` → `RenderProduct` → `orderedVars` → `RenderVar`:
//! which files a render writes and which AOVs go in them ([`AovRequest`]).
//!
//! A small resolver over the typed `openusd-schemas` views rather than
//! `openusd_schemas::render::compute_render_spec`, which takes no time code:
//! `productName` is *varying* precisely so a shot can time-sample it per
//! frame, and every attribute here is read at the render's time code like
//! every other one crust reads. It follows `UsdRenderComputeSpec` step by
//! step otherwise — a product starts from the settings' base attributes and
//! overrides only what it authors, vars keep `orderedVars` order, a var
//! targeted twice in one product is used once — and a test pins the two to
//! the same answer on a time-invariant stage.
//!
//! The vocabulary (what a `sourceName` means) is [`crate::aov`]'s. Everything
//! refused here — a deep product, a mismatched camera, an unknown source, a
//! type that cannot hold its source — is refused with one `WARN` naming the
//! prim, and gets no file or channel rather than a black one that looks
//! valid.

use crate::warning;
use std::cell::RefCell;
use std::collections::HashMap;

use moho::openusd::sdf;
use moho::openusd::usd::Stage;
use moho::scene_index::SceneIndex;
use moho::tokens::prim_type;
use openusd_schemas::render::AspectRatioConformPolicy;
use tracing::debug;

use crate::aov::{
    Accumulation, AovProduct, AovRequest, AovSource, AovVar, FilterSource, Precision, PrimvarRole,
    PrimvarSource,
};
use crate::region::PixelRect;

use super::attrs::{decode_bool, decode_number};
use super::index::data::PrimData;
use super::settings::render_settings_path;

const DRIVER_PARAMETERS: &str = "driver:parameters:";

/// How the render camera's aperture is fitted to the image: the first
/// product's resolved `aspectRatioConformPolicy` and `pixelAspectRatio`,
/// which [`super::camera`]'s frame read applies before any field of view is
/// derived. Kept beside the resolution rather than on
/// [`crate::tracer::RenderSettings`], which holds no USD vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Framing {
    /// `expandAperture` when nothing authors one.
    pub(super) policy: AspectRatioConformPolicy,
    /// The width of a pixel over its height, as authored: a
    /// [`valid_pixel_aspect`], `1` when nothing authors one (a refused value
    /// reads as unauthored). `adjustPixelAspectRatio` replaces it with the one the
    /// aperture needs ([`super::camera`]'s conform).
    pub(super) pixel_aspect: f32,
}

impl Framing {
    /// Whether two framings frame the same image. `adjustPixelAspectRatio`
    /// derives the pixel aspect from the aperture, whatever is authored, so
    /// under it only the policy counts.
    fn frames_like(&self, other: &Framing) -> bool {
        self.policy == other.policy
            && (self.policy == AspectRatioConformPolicy::AdjustPixelAspectRatio
                || self.pixel_aspect == other.pixel_aspect)
    }
}

/// Whether `pixel_aspect` is one the render can frame and write: a normal
/// number between 1e-6 and 1e6, the range an EXR header's
/// `pixelAspectRatio` may hold (the `exr` crate refuses to write any
/// other, and the render's files with it).
pub(super) fn valid_pixel_aspect(pixel_aspect: f32) -> bool {
    pixel_aspect.is_normal() && (1e-6..=1e6).contains(&pixel_aspect)
}

impl Default for Framing {
    fn default() -> Self {
        Framing {
            policy: AspectRatioConformPolicy::default(),
            pixel_aspect: 1.0,
        }
    }
}

/// The stage's products, and the camera, resolution, data window, framing
/// and motion-blur and depth-of-field switches the render takes from the
/// first of them.
#[derive(Debug)]
pub(super) struct RenderProducts {
    pub(super) request: AovRequest,
    /// What each filter var's `crust:filter` targets, by the var's prim
    /// path: resolved into its [`AovSource::Filter`] by the precomp import.
    pub(super) filter_targets: HashMap<String, FilterTarget>,
    /// The primvars the request's primvar vars read, which the geometry
    /// import keeps (and nothing else for them).
    pub(super) primvars: PrimvarKeep,
    /// The first product's resolved camera — the settings' own when the
    /// product authors none.
    pub(super) camera: Option<sdf::Path>,
    /// The first product's resolved resolution, when the settings or the
    /// product author one.
    pub(super) resolution: Option<(usize, usize)>,
    /// The first product's resolved `dataWindowNDC` (`xmin, ymin, xmax,
    /// ymax`, bottom-left origin), when the settings or the product author
    /// one; [`region_from_ndc`] turns it into the render's region.
    pub(super) data_window: Option<[f32; 4]>,
    /// The first product's resolved conform policy.
    pub(super) framing: Framing,
    /// Whether moving geometry is motion blurred: `false` when the first
    /// product (else the settings prim) authors `disableMotionBlur = true`
    /// or its deprecated synonym `instantaneousShutter = true`. The stage's
    /// motion stays in the scene either way (`RenderSettings::with_motion_blur`).
    pub(super) motion_blur: bool,
    /// Whether the sharp beauty is posed at the frame rather than at shutter
    /// open: `disableMotionBlur = true`, which UsdRender defines as a `[0, 0]`
    /// shutter at the current time code. `instantaneousShutter` alone keeps
    /// shutter open.
    pub(super) sharp_at_frame: bool,
    /// Whether the camera's lens is kept: `false` when the first product
    /// (else the settings prim) authors `disableDepthOfField = true`, and the
    /// camera is built as a pinhole (`RenderSettings::with_depth_of_field`).
    pub(super) depth_of_field: bool,
}

/// Hand-written for two fields: a stage that authors nothing blurs.
impl Default for RenderProducts {
    fn default() -> Self {
        RenderProducts {
            request: AovRequest::default(),
            filter_targets: HashMap::new(),
            primvars: PrimvarKeep::default(),
            camera: None,
            resolution: None,
            data_window: None,
            framing: Framing::default(),
            motion_blur: true,
            sharp_at_frame: false,
            depth_of_field: true,
        }
    }
}

/// The base attributes a product inherits from the settings and may
/// override — the ones crust renders with.
#[derive(Debug, Clone, PartialEq)]
struct Base {
    camera: Option<sdf::Path>,
    resolution: Option<(usize, usize)>,
    data_window: Option<[f32; 4]>,
    conform: AspectRatioConformPolicy,
    pixel_aspect: f32,
    /// `disableMotionBlur` and `instantaneousShutter`, each resolved on its
    /// own: a product that authors one of them inherits the other.
    disable_motion_blur: bool,
    instantaneous_shutter: bool,
    disable_depth_of_field: bool,
}

fn read_resolution(view: &PrimData) -> Option<(usize, usize)> {
    let v = view.value("resolution")?.try_as_vec_2i()?;
    (v.x > 0 && v.y > 0).then_some((v.x as usize, v.y as usize))
}

fn read_data_window(view: &PrimData) -> Option<[f32; 4]> {
    let v = view.value("dataWindowNDC")?.try_as_vec_4f()?;
    Some([v.x, v.y, v.z, v.w])
}

/// The authored `aspectRatioConformPolicy`; `None` when unauthored, or when
/// it names none of the five policies, which is refused at the prim that
/// authors it.
fn read_conform_policy(view: &PrimData) -> Option<AspectRatioConformPolicy> {
    let value = view.value("aspectRatioConformPolicy")?;
    match AspectRatioConformPolicy::try_from(value.clone()) {
        Ok(policy) => Some(policy),
        Err(_) => {
            let path = view.path();
            warning!(
                ProductInvalidConformPolicy,
                at = path,
                "{path}: aspectRatioConformPolicy = {} is not one of expandAperture, \
                 cropAperture, adjustApertureWidth, adjustApertureHeight or \
                 adjustPixelAspectRatio; read as unauthored",
                describe_value(&value),
            );
            None
        }
    }
}

/// An authored value as a warning quotes it: a token in quotes, anything
/// else as its debug form.
fn describe_value(value: &sdf::Value) -> String {
    match value {
        sdf::Value::Token(t) => format!("{:?}", t.as_str()),
        other => format!("{other:?}"),
    }
}

/// The authored `pixelAspectRatio`; `None` when unauthored, or when it is
/// not a [`valid_pixel_aspect`], which is refused at the prim that authors
/// it.
fn read_pixel_aspect(view: &PrimData) -> Option<f32> {
    let value = view.value("pixelAspectRatio")?;
    match decode_number(value.clone()) {
        Some(a) if valid_pixel_aspect(a) => Some(a),
        _ => {
            let path = view.path();
            warning!(
                ProductInvalidPixelAspectRatio,
                at = path,
                "{path}: pixelAspectRatio = {} is not a number between 1e-6 and 1e6, which an \
                 EXR header can hold; read as unauthored",
                describe_value(&value),
            );
            None
        }
    }
}

fn read_camera(view: &PrimData) -> Option<sdf::Path> {
    view.targets("camera").into_iter().next()
}

impl Base {
    /// Nothing authored anywhere: the first camera, the default resolution,
    /// motion blur and depth of field on.
    fn unauthored() -> Base {
        Base {
            camera: None,
            resolution: None,
            data_window: None,
            conform: AspectRatioConformPolicy::ExpandAperture,
            pixel_aspect: 1.0,
            disable_motion_blur: false,
            instantaneous_shutter: false,
            disable_depth_of_field: false,
        }
    }

    /// `view`'s authored opinions over `fallback`.
    fn resolve(view: &PrimData, fallback: &Base) -> Base {
        Base {
            camera: read_camera(view).or_else(|| fallback.camera.clone()),
            resolution: read_resolution(view).or(fallback.resolution),
            data_window: read_data_window(view).or(fallback.data_window),
            conform: read_conform_policy(view).unwrap_or(fallback.conform),
            pixel_aspect: read_pixel_aspect(view).unwrap_or(fallback.pixel_aspect),
            disable_motion_blur: view
                .value("disableMotionBlur")
                .and_then(decode_bool)
                .unwrap_or(fallback.disable_motion_blur),
            instantaneous_shutter: view
                .value("instantaneousShutter")
                .and_then(decode_bool)
                .unwrap_or(fallback.instantaneous_shutter),
            disable_depth_of_field: view
                .value("disableDepthOfField")
                .and_then(decode_bool)
                .unwrap_or(fallback.disable_depth_of_field),
        }
    }

    /// Whether the render blurs moving geometry: either flag turns it off.
    fn motion_blur(&self) -> bool {
        !(self.disable_motion_blur || self.instantaneous_shutter)
    }

    /// Whether the sharp beauty is posed at the frame: `disableMotionBlur`
    /// wins over `instantaneousShutter`, which poses it at shutter open.
    fn sharp_at_frame(&self) -> bool {
        self.disable_motion_blur
    }

    /// How the render treats moving geometry, as a product's warning names it.
    fn blur_mode(&self) -> &'static str {
        match (self.motion_blur(), self.sharp_at_frame()) {
            (true, _) => "on",
            (false, true) => "off (sharp at the frame)",
            (false, false) => "off (sharp at shutter open)",
        }
    }

    /// Whether the render camera keeps its lens.
    fn depth_of_field(&self) -> bool {
        !self.disable_depth_of_field
    }

    fn framing(&self) -> Framing {
        Framing {
            policy: self.conform,
            pixel_aspect: self.pixel_aspect,
        }
    }

    /// Whether two products can be rendered by one render: the same camera
    /// and resolution. The shutter and depth-of-field flags, the data
    /// window and the framing are not part of it — the render follows the first product's, and a later one that
    /// differs is warned about, not refused.
    fn same_render(&self, other: &Base) -> bool {
        self.camera == other.camera && self.resolution == other.resolution
    }
}

/// The primvars a render's primvar vars read: `(name, components)` without
/// the `primvars:` prefix, deduplicated, in first-use order. An entry's index
/// is the [`PrimvarSource::index`] of every var reading it.
///
/// It also remembers, for the import that reads geometry with it, which
/// ancestors author a kept primvar crust does not inherit, and which entries
/// have been warned about ([`PrimvarKeep::report_inherited`]); two keep sets
/// are equal when their entries are.
#[derive(Debug, Clone, Default)]
pub(crate) struct PrimvarKeep {
    pub(crate) entries: Vec<KeptPrimvar>,
    /// `(entry, ancestor path)` → whether that prim authors the primvar.
    authored_on: RefCell<HashMap<(usize, String), bool>>,
    /// Per entry, whether its not-inherited warning was raised.
    warned: RefCell<Vec<bool>>,
}

impl PartialEq for PrimvarKeep {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for PrimvarKeep {}

/// One primvar the geometry import keeps for the AOVs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptPrimvar {
    /// Without the `primvars:` prefix.
    pub(crate) name: String,
    /// The components every var reading it asks for: 1, 2 or 3.
    pub(crate) components: u8,
    /// The RenderVar prims reading it, for the warnings that name them.
    pub(crate) vars: Vec<String>,
}

impl KeptPrimvar {
    /// The vars reading it and the verb: `/Render/Pref asks for`.
    pub(crate) fn asking(&self) -> String {
        let verb = if self.vars.len() == 1 {
            "asks for"
        } else {
            "ask for"
        };
        format!("{} {verb}", self.vars.join(", "))
    }
}

impl PrimvarKeep {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What the geometry import keeps: each entry's `(name, components)`.
    pub(crate) fn key(&self) -> Vec<(String, u8)> {
        self.entries
            .iter()
            .map(|e| (e.name.clone(), e.components))
            .collect()
    }

    /// Kept primvar `i`, which geometry prim `prim` does not author: when an
    /// ancestor authors it, warns — once per entry — that it is not
    /// inherited. Walks the ancestors once per `(entry, ancestor)`.
    pub(crate) fn report_inherited(&self, prim: &PrimData, i: usize) {
        if self.warned.borrow().get(i).copied().unwrap_or(false) {
            return;
        }
        let name = format!("primvars:{}", self.entries[i].name);
        let mut at = prim.parent();
        while let Some(a) = at {
            let key = (i, a.path().to_string());
            let authored = *self
                .authored_on
                .borrow_mut()
                .entry(key)
                .or_insert_with(|| a.value(&name).is_some());
            if authored {
                self.warn_not_inherited(i, Some(prim), &a, "an ancestor");
                return;
            }
            at = a.parent();
        }
    }

    /// An instance prim or a `PointInstancer` (`what`) placing geometry:
    /// warns, once per entry, about each kept primvar it or an ancestor
    /// authors, which the placed geometry does not inherit.
    pub(crate) fn report_placement(&self, placement: &PrimData, what: &str) {
        for i in 0..self.entries.len() {
            if self.warned.borrow().get(i).copied().unwrap_or(false) {
                continue;
            }
            let name = format!("primvars:{}", self.entries[i].name);
            if placement.value(&name).is_some() {
                self.warn_not_inherited(i, None, placement, what);
            } else {
                self.report_inherited(placement, i);
            }
        }
    }

    /// Warns that kept primvar `i`, which `what` authors on `on` (above the
    /// geometry prim `below`, when one asked), is not inherited.
    fn warn_not_inherited(&self, i: usize, below: Option<&PrimData>, on: &PrimData, what: &str) {
        let e = &self.entries[i];
        let above = below.map_or(String::new(), |b| format!(" above {}", b.path()));
        warning!(
            PrimvarNotInherited,
            at = on.path(),
            "{} primvar {:?}, which {what} authors on {}{above}; crust does not inherit \
             primvars yet, so the prims it would reach read the clear value",
            e.asking(),
            e.name,
            on.path(),
        );
        let mut warned = self.warned.borrow_mut();
        if warned.len() < self.entries.len() {
            warned.resize(self.entries.len(), false);
        }
        warned[i] = true;
    }

    /// The index of `(name, components)`, added if new, with `var` among
    /// the vars reading it.
    fn index_of(&mut self, name: &str, components: u8, var: &sdf::Path) -> u16 {
        let i = match self
            .entries
            .iter()
            .position(|e| e.name == name && e.components == components)
        {
            Some(i) => i,
            None => {
                self.entries.push(KeptPrimvar {
                    name: name.to_owned(),
                    components,
                    vars: Vec::new(),
                });
                self.warned.borrow_mut().push(false);
                self.entries.len() - 1
            }
        };
        let var = var.to_string();
        if !self.entries[i].vars.contains(&var) {
            self.entries[i].vars.push(var);
        }
        i as u16
    }
}

/// Resolves the render settings prim's products. Empty when the stage has no
/// settings prim or the prim authors no `products` — the "write the single
/// beauty EXR" case.
///
/// The precomp is read too when `stage_path` (the stage's file, which its
/// kernel asset paths resolve against) is given; a reader that only wants
/// the products' camera and framing (`crust ls`) passes `None`.
pub(super) fn import_render_products(
    stage: &Stage,
    scene: &dyn SceneIndex,
    stage_path: Option<&std::path::Path>,
    pass: Option<&sdf::Path>,
) -> RenderProducts {
    let Some(path) = render_settings_path(scene, pass) else {
        return RenderProducts::default();
    };
    let settings = PrimData::at(scene, &path);
    if settings.ty() != prim_type::RENDER_SETTINGS {
        return RenderProducts::default();
    }
    let mut out = read_products(scene, &path, &settings);
    if let Some(stage_path) = stage_path {
        out.request.precomp = super::precomp::import_precomp(
            stage,
            scene,
            stage_path,
            &path,
            &mut out.request,
            &out.filter_targets,
        );
    }
    out
}

/// The products of the settings prim `path`, before the precomp is read.
fn read_products(scene: &dyn SceneIndex, path: &sdf::Path, settings: &PrimData) -> RenderProducts {
    let path = path.clone();
    let targets = settings.forwarded_targets("products");
    let base = Base::resolve(settings, &Base::unauthored());
    if targets.is_empty() {
        return RenderProducts {
            request: AovRequest::default(),
            filter_targets: HashMap::new(),
            primvars: PrimvarKeep::default(),
            motion_blur: base.motion_blur(),
            sharp_at_frame: base.sharp_at_frame(),
            depth_of_field: base.depth_of_field(),
            framing: base.framing(),
            camera: base.camera,
            resolution: base.resolution,
            data_window: base.data_window,
        };
    }

    let mut out = RenderProducts {
        request: AovRequest::default(),
        filter_targets: HashMap::new(),
        primvars: PrimvarKeep::default(),
        camera: base.camera.clone(),
        resolution: base.resolution,
        data_window: base.data_window,
        framing: base.framing(),
        motion_blur: base.motion_blur(),
        sharp_at_frame: base.sharp_at_frame(),
        depth_of_field: base.depth_of_field(),
    };
    // The render's own base: the first accepted product's.
    let mut render_base: Option<Base> = None;
    // A var shared by several products is resolved, and warned about, once.
    let mut vars: HashMap<sdf::Path, Option<AovVar>> = HashMap::new();
    let mut lpes: Vec<String> = Vec::new();
    // Expressions refused once, so a var shared by products warns once.
    let mut refused: Vec<String> = Vec::new();

    for product_path in targets {
        let product = PrimData::at(scene, &product_path);
        if product.ty() != prim_type::RENDER_PRODUCT {
            warning!(
                ProductNotARenderProduct,
                at = product_path,
                "{path}.products targets {product_path}, which is not a RenderProduct; skipped"
            );
            continue;
        }
        let prim = &product;
        let product_type = prim
            .custom_token("productType")
            .unwrap_or_else(|| "raster".to_owned());
        if product_type != "raster" {
            warning!(
                ProductUnsupportedType,
                at = product_path,
                "{product_path}: productType {product_type:?} is not supported (only \
                 \"raster\"); no file is written for it"
            );
            continue;
        }
        // The camera's existence is not checked here: this is the index
        // stage, with payloads unloaded, and a shot camera under a payload
        // (the production case) is not composed on it. A camera that really
        // is missing is the traversal's to find, which warns and falls back
        // to the first camera (`CameraChoice::Settings`).
        let resolved = Base::resolve(&product, &base);
        match &render_base {
            None => render_base = Some(resolved.clone()),
            Some(first) if !first.same_render(&resolved) => {
                warning!(
                    ProductCameraMismatch,
                    at = product_path,
                    "{product_path}: renders through {} at {}, but the render is {} at {} \
                     (the first product's); crust renders one camera and resolution per \
                     stage, so no file is written for it",
                    describe_camera(&resolved.camera),
                    describe_resolution(resolved.resolution),
                    describe_camera(&first.camera),
                    describe_resolution(first.resolution),
                );
                continue;
            }
            Some(first) if first.blur_mode() != resolved.blur_mode() => {
                // One shutter per stage, like one camera: the file is still
                // written, with the first product's blur.
                warning!(
                    ProductMotionBlurMismatch,
                    at = product_path,
                    "{product_path}: asks for motion blur {}, but the render has it {} (the \
                     first product's); its file is written with that",
                    resolved.blur_mode(),
                    first.blur_mode(),
                );
            }
            Some(_) => {}
        }
        // Checked on its own, like the region: a product may differ from
        // the first in both its shutter and its lens.
        if let Some(first) = &render_base
            && first.depth_of_field() != resolved.depth_of_field()
        {
            // One camera per render: the file is still written, with the
            // first product's lens.
            let state = |on: bool| if on { "on" } else { "off" };
            warning!(
                ProductDepthOfFieldMismatch,
                at = product_path,
                "{product_path}: asks for depth of field {}, but the render has it {} (the \
                 first product's); its file is written with that",
                state(resolved.depth_of_field()),
                state(first.depth_of_field()),
            );
        }
        // Compared as they render: an unauthored window is the full frame,
        // like an authored `(0, 0, 1, 1)`.
        if let Some(first) = &render_base
            && window_or_full(first.data_window) != window_or_full(resolved.data_window)
        {
            // One region per render, like one shutter: the file is still
            // written, over the first product's region.
            warning!(
                ProductRegionMismatch,
                at = product_path,
                "{product_path}: asks for dataWindowNDC {}, but the render's is {} (the first \
                 product's); its file is written with that",
                describe_window(resolved.data_window),
                describe_window(first.data_window),
            );
        }
        // One framing per render, like one region: the file is still
        // written, framed by the first product's policy and pixel aspect.
        if let Some(first) = &render_base
            && !first.framing().frames_like(&resolved.framing())
        {
            warning!(
                ProductFramingMismatch,
                at = product_path,
                "{product_path}: asks for aspectRatioConformPolicy {:?} with pixelAspectRatio \
                 {}, but the render's is {:?} with {} (the first product's); its file is \
                 written with that framing",
                resolved.conform.as_token(),
                resolved.pixel_aspect,
                first.conform.as_token(),
                first.pixel_aspect,
            );
        }

        let name = prim.custom_token("productName").unwrap_or_default();
        let mut seen = Vec::new();
        let mut product_vars = Vec::new();
        for var_path in product.forwarded_targets("orderedVars") {
            if seen.contains(&var_path) {
                debug!("{product_path}: {var_path} is targeted twice; using it once");
                continue;
            }
            seen.push(var_path.clone());
            let var = vars
                .entry(var_path.clone())
                .or_insert_with(|| {
                    resolve_var(scene, &var_path, &mut out.primvars, &mut out.filter_targets)
                })
                .clone();
            // One DFA holds every expression of the render, each accepting
            // as one bit of a u64 and all sharing a bounded number of
            // states: an expression is accepted only if the render's set
            // still compiles with it, so the renderer's compile cannot fail.
            if let Some(v) = &var
                && let Some(e) = &v.expression
                && !lpes.contains(e)
            {
                if refused.contains(e) {
                    continue;
                }
                let mut set: Vec<&str> = lpes.iter().map(String::as_str).collect();
                set.push(e);
                if let Err(err) = crate::lpe::Lpe::compile(&set) {
                    warning!(
                        LpeInvalid,
                        at = var_path,
                        "{var_path}: light path expression {e:?} refused: {err}"
                    );
                    refused.push(e.clone());
                    continue;
                }
                lpes.push(e.clone());
            }
            product_vars.extend(var);
        }
        let attributes = driver_attributes(prim);
        debug!(
            "{product_path}: {:?} with {} var(s): {}",
            name,
            product_vars.len(),
            product_vars
                .iter()
                .map(|v| format!("{} = {}", v.name, v.source.name()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        out.request.products.push(AovProduct {
            prim_path: product_path.to_string(),
            name,
            vars: product_vars,
            attributes,
        });
    }
    if let Some(first) = render_base {
        out.motion_blur = first.motion_blur();
        out.sharp_at_frame = first.sharp_at_frame();
        out.depth_of_field = first.depth_of_field();
        out.framing = first.framing();
        out.camera = first.camera;
        out.resolution = first.resolution;
        out.data_window = first.data_window;
    }
    out
}

/// `UsdRender`'s fallback `dataWindowNDC`: the whole frame.
const FULL_WINDOW: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The window a render uses: the authored one, else the full frame.
fn window_or_full(window: Option<[f32; 4]>) -> [f32; 4] {
    window.unwrap_or(FULL_WINDOW)
}

fn describe_window(window: Option<[f32; 4]>) -> String {
    let [x0, y0, x1, y1] = window_or_full(window);
    format!("({x0}, {y0}, {x1}, {y1})")
}

/// The render region a `dataWindowNDC` selects on a `width` × `height`
/// frame: the pixels whose centres lie inside the window, `xmin ≤ (x + ½)/W
/// < xmax` and `ymin ≤ 1 − (y + ½)/H < ymax` — NDC's `y` grows upwards from
/// the bottom-left, the region's downwards from the top-left. `None` is the
/// full frame.
///
/// A window reaching outside [0, 1] (overscan) is clipped to the frame, with
/// one warning; one that selects no pixel — a window wholly outside the frame
/// included — or is not finite, is refused with one warning and the full
/// frame renders. No window gets two.
pub(super) fn region_from_ndc(window: [f32; 4], width: usize, height: usize) -> Option<PixelRect> {
    let text = describe_window(Some(window));
    if window == FULL_WINDOW {
        return None;
    }
    if !window.iter().all(|c| c.is_finite()) {
        warning!(
            ProductInvalidDataWindow,
            "dataWindowNDC = {text} is not finite; rendering the full frame"
        );
        return None;
    }
    let [xmin, ymin, xmax, ymax] = window.map(f64::from);
    let (w, h) = (width as f64, height as f64);
    // The first pixel whose centre is at or past `c` (for the `≤` bounds),
    // and the first whose centre is past `c` (for the strict ones), each
    // clamped to the frame.
    let at_or_past = |c: f64, n: f64| (c * n - 0.5).ceil().clamp(0.0, n) as usize;
    let past = |c: f64, n: f64| ((c * n - 0.5).floor() + 1.0).clamp(0.0, n) as usize;
    let region = PixelRect::new(
        at_or_past(xmin, w),
        past(1.0 - ymax, h),
        at_or_past(xmax, w),
        past(1.0 - ymin, h),
    );
    if region.is_empty() {
        warning!(
            ProductInvalidDataWindow,
            "dataWindowNDC = {text} selects no pixel of the {width}x{height} frame; rendering the full frame"
        );
        return None;
    }
    // Only once the clipped window is known to select something: a window
    // wholly outside the frame is the empty case above, not a clipped one.
    if window.iter().any(|c| !(0.0..=1.0).contains(c)) {
        warning!(
            ProductDataWindowClipped,
            "dataWindowNDC = {text} reaches outside the frame; overscan is not supported, \
             so it is clipped to it"
        );
    }
    Some(region)
}

fn describe_camera(camera: &Option<sdf::Path>) -> String {
    camera
        .as_ref()
        .map_or_else(|| "the first camera".to_owned(), ToString::to_string)
}

fn describe_resolution(resolution: Option<(usize, usize)>) -> String {
    resolution.map_or_else(
        || "the default resolution".to_owned(),
        |(w, h)| format!("{w}x{h}"),
    )
}

/// A product's authored `driver:parameters:*` text values (outside the
/// `aov:` namespace, which configures vars), for the EXR header:
/// `driver:parameters:OpenEXR:<key>` as `<key>`, the rest by their name
/// without the prefix (`artist`, `comment`).
fn driver_attributes(prim: &PrimData) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = prim
        .attribute_names()
        .into_iter()
        .filter_map(|name| {
            let name = name.as_str();
            let key = name.strip_prefix(DRIVER_PARAMETERS)?;
            if key.starts_with("aov:") {
                return None;
            }
            let value = prim.custom_token(name)?;
            let key = key.strip_prefix("OpenEXR:").unwrap_or(key);
            Some((key.to_owned(), value))
        })
        .collect();
    out.sort();
    out
}

/// A float attribute authored as any numeric type — Houdini writes
/// `clearValue` as a float, hand-written files as an int.
fn custom_number(prim: &PrimData, name: &str) -> Option<f32> {
    prim.value(name).and_then(decode_number)
}

/// Components and precision of an Sdf type name or a Houdini
/// `aov:format`, or `None` for anything else. The whole name must be one of
/// the spellings below — a prefix match would let `floatgarbage` through as
/// a float:
///
/// - `float`, `half`, `double`, `int`, `uint`, alone (one component) or
///   followed by `2`, `3` or `4` (Houdini's `float3`, `half4`, ...);
/// - `color3`/`color4`, `normal3`, `point3`, `vector3`, `texCoord2`/
///   `texCoord3`, followed by `f`, `h` or `d`.
///
/// `int`/`uint` are UINT, `half` and the `h` suffix HALF, everything else
/// FLOAT (a `double` is accumulated and written as f32).
pub(super) fn parse_data_type(name: &str) -> Option<(usize, Precision)> {
    const SCALARS: &[(&str, Precision)] = &[
        ("float", Precision::Float),
        ("half", Precision::Half),
        ("double", Precision::Float),
        ("int", Precision::Uint),
        ("uint", Precision::Uint),
    ];
    const ROLES: &[(&str, &[usize])] = &[
        ("color", &[3, 4]),
        ("normal", &[3]),
        ("point", &[3]),
        ("vector", &[3]),
        ("texCoord", &[2, 3]),
    ];
    let digit = |rest: &str| -> Option<usize> {
        let n = rest.parse::<usize>().ok()?;
        (rest.len() == 1 && (2..=4).contains(&n)).then_some(n)
    };
    for (family, precision) in SCALARS {
        if let Some(rest) = name.strip_prefix(family) {
            if rest.is_empty() {
                return Some((1, *precision));
            }
            return digit(rest).map(|n| (n, *precision));
        }
    }
    for (family, counts) in ROLES {
        if let Some(rest) = name.strip_prefix(family) {
            let mut chars = rest.chars();
            let (Some(n), Some(suffix), None) = (chars.next(), chars.next(), chars.next()) else {
                return None;
            };
            let n = n.to_digit(10)? as usize;
            if !counts.contains(&n) {
                return None;
            }
            let precision = match suffix {
                'f' | 'd' => Precision::Float,
                'h' => Precision::Half,
                _ => return None,
            };
            return Some((n, precision));
        }
    }
    None
}

/// Whether `source` can be written as `components` channels of `precision`.
/// A primvar's width is its data type's, so any one to three components fit,
/// as integers too.
fn type_fits(source: AovSource, components: usize, precision: Precision) -> bool {
    let components_fit = match source {
        AovSource::Color | AovSource::Lpe => components == 3 || components == 4,
        AovSource::Primvar(_) | AovSource::Filter(_) => (1..=3).contains(&components),
        s => components == s.components(),
    };
    let precision_fits = precision != Precision::Uint
        || matches!(source, AovSource::SampleCount | AovSource::Primvar(_));
    components_fit && precision_fits
}

/// The accumulation a var's authored attributes ask for, first match wins:
/// Hydra's `multiSampled`, then Arnold, Karma and RenderMan filter
/// attributes. `None` when none is authored (or the one authored names a
/// rule crust does not have, which is warned).
fn authored_accumulation(prim: &PrimData) -> Option<Accumulation> {
    if let Some(multi) = prim.custom_bool("driver:parameters:aov:multiSampled") {
        return Some(if multi {
            Accumulation::Filtered
        } else {
            Accumulation::Closest
        });
    }
    if let Some(filter) = prim.custom_token("arnold:filter") {
        return Some(if filter == "closest_filter" {
            Accumulation::Closest
        } else {
            Accumulation::Filtered
        });
    }
    if let Some(filter) = prim.custom_token("driver:parameters:aov:filter") {
        let f = filter.trim_start();
        return Some(
            if f.starts_with("[\"closest\"") || f.starts_with("closest") {
                Accumulation::Closest
            } else {
                Accumulation::Filtered
            },
        );
    }
    for name in ["ri:accumulationRule", "ri:displayChannel:filter"] {
        if let Some(rule) = prim.custom_token(name) {
            return match rule.as_str() {
                "zmin" => Some(Accumulation::Closest),
                "filter" | "" => Some(Accumulation::Filtered),
                other => {
                    warning!(
                        AovUnknownAccumulation,
                        at = prim.path(),
                        "{}: {name} = {other:?} is not supported (only \"zmin\" and the \
                         filtered default); using the source's default",
                        prim.path()
                    );
                    None
                }
            };
        }
    }
    None
}

/// One RenderVar as a channel layer, or `None` (warned) when crust cannot
/// honour it.
fn resolve_var(
    scene: &dyn SceneIndex,
    path: &sdf::Path,
    keep: &mut PrimvarKeep,
    filters: &mut HashMap<String, FilterTarget>,
) -> Option<AovVar> {
    let prim = PrimData::at(scene, path);
    if prim.ty() != prim_type::RENDER_VAR {
        warning!(
            AovNotARenderVar,
            at = path,
            "orderedVars targets {path}, which is not a RenderVar; skipped"
        );
        return None;
    }
    let prim_name = path.name().unwrap_or_default().to_owned();
    let name = prim
        .custom_token("driver:parameters:aov:name")
        .filter(|n| !n.is_empty())
        .unwrap_or(prim_name);
    let channel_prefix = prim
        .custom_token("driver:parameters:aov:channel_prefix")
        .or_else(|| prim.custom_token("driver:parameters:aov:husk:channel_prefix"));

    // A filter output: its `sourceType` and `sourceName` are ignored.
    if let Some(target) = prim.targets("crust:filter").into_iter().next() {
        return resolve_filter_var(&prim, path, name, channel_prefix, target, filters);
    }
    let source_type = prim
        .custom_token("sourceType")
        .unwrap_or_else(|| "raw".to_owned());
    let source_name = prim.custom_token("sourceName").unwrap_or_default();
    let mut expression = None;
    let mut primvar = None;
    let mut raw = false;
    let raw_authored = prim.custom_bool("crust:aov:raw") == Some(true);
    let variance = prim.custom_bool("crust:aov:variance") == Some(true);
    if variance && source_type != "lpe" {
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: crust:aov:variance applies to light path expressions (sourceType \
             \"lpe\"), not sourceType {source_type:?}; no channel written"
        );
        return None;
    }
    let source = match source_type.as_str() {
        "raw" => {
            let lookup = if source_name.is_empty() {
                &name
            } else {
                &source_name
            };
            if raw_authored {
                warning!(
                    AovRawIgnored,
                    at = path,
                    "{path}: crust:aov:raw applies to light path expressions \
                     (sourceType \"lpe\"); ignored on {lookup:?}"
                );
            }
            if let Some(expr) = crate::aov::raw_light_expression(lookup) {
                // `rawLight` …: a fixed expression, divided by the diffuse
                // filter. It starts with a diffuse reflection by construction.
                expression = Some(expr.to_owned());
                raw = true;
                AovSource::Lpe
            } else {
                match AovSource::from_raw(lookup) {
                    Some(s) => s,
                    None if AovSource::is_planned(lookup) => {
                        warning!(
                            AovUnsupportedSource,
                            at = path,
                            "{path}: source {lookup:?} is not supported yet; no channel written"
                        );
                        return None;
                    }
                    None => {
                        warning!(
                            AovUnknownSource,
                            at = path,
                            "{path}: unknown raw source {lookup:?}; no channel written (crust's \
                         AOV names are listed in the user documentation, usd/aovs)"
                        );
                        return None;
                    }
                }
            }
        }
        "lpe" => {
            let expr = crate::lpe::strip_prefix(&source_name);
            if let Err(e) = crate::lpe::validate(expr) {
                warning!(
                    LpeInvalid,
                    at = path,
                    "{path}: light path expression {expr:?}: {e}; no channel written"
                );
                return None;
            }
            if raw_authored {
                // Raw light divides by the diffuse colour of the camera's
                // first hit, which only means something when every path the
                // expression accepts starts by reflecting off it diffusely.
                let lpe = match crate::lpe::Lpe::compile(&[expr]) {
                    Ok(lpe) => lpe,
                    Err(e) => {
                        warning!(
                            LpeInvalid,
                            at = path,
                            "{path}: light path expression {expr:?}: {e}; no channel written"
                        );
                        return None;
                    }
                };
                if !lpe.starts_with_diffuse_reflection(0) {
                    warning!(
                        AovInvalidRaw,
                        at = path,
                        "{path}: crust:aov:raw needs an expression whose every path starts \
                         with a diffuse reflection (C<RD>…); {expr:?} does not, so no channel \
                         written"
                    );
                    return None;
                }
                raw = true;
            }
            expression = Some(expr.to_owned());
            AovSource::Lpe
        }
        "primvar" => {
            let name = source_name
                .strip_prefix("primvars:")
                .unwrap_or(&source_name);
            if name.is_empty() {
                warning!(
                    AovPrimvarNoName,
                    at = path,
                    "{path}: sourceType \"primvar\" needs a sourceName naming the primvar; no \
                     channel written"
                );
                return None;
            }
            primvar = Some(name.to_owned());
            // Its index, width and role come with the data type, below.
            AovSource::Primvar(PrimvarSource {
                index: 0,
                components: 0,
                role: PrimvarRole::Float,
            })
        }
        "intrinsic" => {
            warning!(
                AovUnsupportedSource,
                at = path,
                "{path}: sourceType \"intrinsic\" is unimplemented in UsdRender itself; no \
                 channel written"
            );
            return None;
        }
        other => {
            warning!(
                AovUnknownSource,
                at = path,
                "{path}: unknown sourceType {other:?}; no channel written"
            );
            return None;
        }
    };

    // An expression's variance is one scalar: unauthored, it is a float.
    let type_name = prim
        .custom_token("driver:parameters:aov:format")
        .or_else(|| prim.custom_token("dataType"))
        .unwrap_or_else(|| if variance { "float" } else { "color3f" }.to_owned());
    let Some((components, precision)) = parse_data_type(&type_name) else {
        warning!(
            AovTypeMismatch,
            at = path,
            "{path}: data type {type_name:?} is not a numeric type; no channel written"
        );
        return None;
    };
    let source = match (source, &primvar) {
        (AovSource::Primvar(_), Some(name)) => {
            if components == 4 {
                warning!(
                    AovPrimvarFourComponents,
                    at = path,
                    "{path}: primvar {name:?} as {type_name:?}: four-component primvar vars are \
                     not supported yet; no channel written"
                );
                return None;
            }
            // The role is the `dataType`'s (Houdini's `aov:format` names only
            // the storage, `float3` for a `point3f`).
            let role = prim
                .custom_token("dataType")
                .and_then(|t| PrimvarRole::of_type(&t))
                .or_else(|| PrimvarRole::of_type(&type_name))
                .unwrap_or(PrimvarRole::Float);
            let components = components as u8;
            AovSource::Primvar(PrimvarSource {
                index: keep.index_of(name, components, path),
                components,
                role,
            })
        }
        (source, _) => source,
    };
    if variance && (components != 1 || precision == Precision::Uint) {
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: the variance of {:?} is one float channel and cannot be written as \
             {type_name:?}; no channel written",
            expression.as_deref().unwrap_or_default()
        );
        return None;
    }
    if !variance && !type_fits(source, components, precision) {
        warning!(
            AovTypeMismatch,
            at = path,
            "{path}: {} cannot be written as {type_name:?}; no channel written",
            source.name()
        );
        return None;
    }

    let default = source.default_accumulation();
    let authored = authored_accumulation(&prim);
    if variance && authored == Some(Accumulation::Closest) {
        // One sample's value per pixel has no sample variance.
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: crust:aov:variance needs every sample, so it cannot be accumulated \
             as Closest; no channel written"
        );
        return None;
    }
    let accumulation = match authored {
        Some(mode) if !source.accepts_accumulation() && mode != default => {
            warning!(
                AovAccumulationIgnored,
                at = path,
                "{path}: {} is a per-pixel quantity and cannot be accumulated as {mode:?}; \
                 using {default:?}",
                source.name()
            );
            default
        }
        Some(mode) => mode,
        None => default,
    };
    let clear = custom_number(&prim, "driver:parameters:aov:clearValue")
        .unwrap_or_else(|| source.default_clear());

    Some(AovVar {
        prim_path: path.to_string(),
        name,
        channel_prefix,
        source,
        components,
        precision,
        accumulation,
        clear,
        expression,
        raw,
        variance,
    })
}

/// What a filter var's `crust:filter` names: the filter prim, and the output
/// (`crust:filter:output`, `None` for the filter's only one). Whether it is
/// radiance rides in the var's placeholder source.
#[derive(Debug, Clone)]
pub(super) struct FilterTarget {
    pub(super) filter: sdf::Path,
    pub(super) output: Option<String>,
}

/// A RenderVar authoring `rel crust:filter`: a filtered layer of one to three
/// float or half channels, whose filter the precomp import resolves. Its
/// source is a placeholder until then.
fn resolve_filter_var(
    prim: &PrimData,
    path: &sdf::Path,
    name: String,
    channel_prefix: Option<String>,
    target: sdf::Path,
    filters: &mut HashMap<String, FilterTarget>,
) -> Option<AovVar> {
    let type_name = prim
        .custom_token("driver:parameters:aov:format")
        .or_else(|| prim.custom_token("dataType"))
        .unwrap_or_else(|| "color3f".to_owned());
    let radiance = prim.custom_bool("crust:filter:radiance").unwrap_or(true);
    let placeholder = |components: usize| {
        AovSource::Filter(FilterSource {
            display: false,
            filter: u16::MAX,
            output: 0,
            components: components as u8,
            radiance,
        })
    };
    let fits = parse_data_type(&type_name).filter(|(c, p)| type_fits(placeholder(*c), *c, *p));
    let Some((components, precision)) = fits else {
        warning!(
            AovTypeMismatch,
            at = path,
            "{path}: a filter output cannot be written as {type_name:?} (one to three float or \
             half channels); no channel written"
        );
        return None;
    };
    if let Some(mode) = authored_accumulation(prim)
        && mode != Accumulation::Filtered
    {
        warning!(
            AovAccumulationIgnored,
            at = path,
            "{path}: a filter output is accumulated as its filter defines it and cannot be \
             {mode:?}; using Filtered"
        );
    }
    filters.insert(
        path.to_string(),
        FilterTarget {
            filter: target,
            output: prim
                .custom_token("crust:filter:output")
                .filter(|o| !o.is_empty()),
        },
    );
    Some(AovVar {
        prim_path: path.to_string(),
        name,
        channel_prefix,
        source: placeholder(components),
        components,
        precision,
        accumulation: Accumulation::Filtered,
        clear: custom_number(prim, "driver:parameters:aov:clearValue").unwrap_or(0.0),
        expression: None,
        raw: false,
        variance: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The products of a stage holding `render` under `/Render` (which must
    /// hold `/Render/settings`), and the warnings resolving them raised.
    fn resolve(name: &str, render: &str) -> (RenderProducts, Vec<crate::Warning>) {
        let dir = std::env::temp_dir().join("crust_products_framing");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(
            &path,
            format!("#usda 1.0\ndef Scope \"Render\"\n{{\n{render}\n}}\n"),
        )
        .expect("write stage");
        let stage = super::super::stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let scope = crate::WarningScope::enter();
        let scene = crate::scene::usd_import::index::input_chain(stage.clone(), None, true);
        let products = import_render_products(&stage, scene.chain.as_ref(), Some(&path), None);
        (products, scope.finish())
    }

    fn policy(name: &str, render: &str) -> AspectRatioConformPolicy {
        let (products, warnings) = resolve(name, render);
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
        products.framing.policy
    }

    fn settings(body: &str) -> String {
        format!("    def RenderSettings \"settings\"\n    {{\n{body}\n    }}\n")
    }

    fn product(name: &str, body: &str) -> String {
        format!(
            "    def RenderProduct \"{name}\"\n    {{\n{body}\n        token productName = \
             \"{name}.exr\"\n    }}\n"
        )
    }

    const HEIGHT: &str =
        "        uniform token aspectRatioConformPolicy = \"adjustApertureHeight\"";
    const WIDTH: &str = "        uniform token aspectRatioConformPolicy = \"adjustApertureWidth\"";

    #[test]
    fn the_conform_policy_resolves_like_the_resolution() {
        use AspectRatioConformPolicy::*;
        assert_eq!(policy("unauthored", &settings("")), ExpandAperture);
        assert_eq!(RenderProducts::default().framing.policy, ExpandAperture);
        assert_eq!(
            policy("settings_only", &settings(HEIGHT)),
            AdjustApertureHeight
        );
        // A product that authors none inherits the settings'.
        let inherits =
            settings(&format!("{WIDTH}\n        rel products = [</Render/a>]")) + &product("a", "");
        assert_eq!(policy("inherited", &inherits), AdjustApertureWidth);
        // The first product's own wins over the settings'.
        let overrides = settings(&format!("{WIDTH}\n        rel products = [</Render/a>]"))
            + &product("a", HEIGHT);
        assert_eq!(policy("overridden", &overrides), AdjustApertureHeight);
    }

    #[test]
    fn a_misspelt_conform_policy_is_refused_as_unauthored() {
        let (products, warnings) = resolve(
            "misspelt",
            &settings("        uniform token aspectRatioConformPolicy = \"adjustApertureHieght\""),
        );
        assert_eq!(
            products.framing.policy,
            AspectRatioConformPolicy::ExpandAperture
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = &warnings[0];
        assert_eq!(w.code, crate::WarningCode::ProductInvalidConformPolicy);
        assert_eq!(w.count, 1);
        assert_eq!(w.prims, ["/Render/settings"]);
        assert!(
            w.message.contains("\"adjustApertureHieght\""),
            "{}",
            w.message
        );

        // On a product, it falls back to the settings' policy.
        let (products, warnings) = resolve(
            "misspelt_product",
            &(settings(&format!("{WIDTH}\n        rel products = [</Render/a>]"))
                + &product(
                    "a",
                    "        uniform token aspectRatioConformPolicy = \"stretch\"",
                )),
        );
        assert_eq!(
            products.framing.policy,
            AspectRatioConformPolicy::AdjustApertureWidth
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].prims, ["/Render/a"]);
    }

    #[test]
    fn a_later_product_with_another_policy_is_written_with_the_first_framing() {
        let (products, warnings) = resolve(
            "framing_mismatch",
            &(settings("        rel products = [</Render/a>, </Render/b>]")
                + &product(
                    "a",
                    "        uniform token aspectRatioConformPolicy = \"expandAperture\"",
                )
                + &product("b", HEIGHT)),
        );
        let names: Vec<&str> = products
            .request
            .products
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, ["a.exr", "b.exr"]);
        assert_eq!(
            products.framing.policy,
            AspectRatioConformPolicy::ExpandAperture
        );
        let mismatch: Vec<_> = warnings
            .iter()
            .filter(|w| w.code == crate::WarningCode::ProductFramingMismatch)
            .collect();
        assert_eq!(mismatch.len(), 1, "{warnings:?}");
        assert_eq!(mismatch[0].count, 1);
        assert_eq!(mismatch[0].prims, ["/Render/b"]);
        assert!(
            warnings
                .iter()
                .all(|w| w.code != crate::WarningCode::ProductCameraMismatch),
            "{warnings:?}"
        );
    }

    const PAR_2: &str = "        uniform float pixelAspectRatio = 2";

    /// `pixelAspectRatio` resolves like the policy, and is no longer warned
    /// about as not honoured.
    #[test]
    fn the_pixel_aspect_resolves_like_the_policy() {
        let aspect = |name: &str, render: &str| {
            let (products, warnings) = resolve(name, render);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            products.framing.pixel_aspect
        };
        assert_eq!(aspect("par_unauthored", &settings("")), 1.0);
        assert_eq!(RenderProducts::default().framing.pixel_aspect, 1.0);
        assert_eq!(aspect("par_settings", &settings(PAR_2)), 2.0);
        let inherits =
            settings(&format!("{PAR_2}\n        rel products = [</Render/a>]")) + &product("a", "");
        assert_eq!(aspect("par_inherited", &inherits), 2.0);
        let overrides = settings(&format!("{PAR_2}\n        rel products = [</Render/a>]"))
            + &product("a", "        uniform float pixelAspectRatio = 0.5");
        assert_eq!(aspect("par_overridden", &overrides), 0.5);
    }

    /// A pixel aspect that is no width over a height, or one no EXR header
    /// can hold, is refused at the prim authoring it and reads as
    /// unauthored. The bounds themselves are accepted.
    #[test]
    fn an_unwritable_pixel_aspect_is_refused() {
        // Past 1e6 either way an EXR header cannot hold it, so no file of
        // the render could be written.
        for (name, value) in [
            ("par_zero", "0"),
            ("par_negative", "-2"),
            ("par_inf", "inf"),
            ("par_huge", "2e6"),
            ("par_tiny", "5e-7"),
        ] {
            let (products, warnings) = resolve(
                name,
                &settings(&format!("        uniform float pixelAspectRatio = {value}")),
            );
            assert_eq!(products.framing.pixel_aspect, 1.0, "{name}");
            assert_eq!(warnings.len(), 1, "{name}: {warnings:?}");
            assert_eq!(
                warnings[0].code,
                crate::WarningCode::ProductInvalidPixelAspectRatio
            );
            assert_eq!(warnings[0].prims, ["/Render/settings"]);
        }
        assert!(valid_pixel_aspect(1e6) && valid_pixel_aspect(1e-6));
    }

    /// Under `adjustPixelAspectRatio` the authored pixel aspect does not
    /// frame anything (the aperture's does), so two products that author
    /// different ones frame alike; under another policy they do not.
    #[test]
    fn an_authored_pixel_aspect_under_adjust_pixel_aspect_ratio_is_no_mismatch() {
        let adjust = "        uniform token aspectRatioConformPolicy = \"adjustPixelAspectRatio\"";
        let (products, warnings) = resolve(
            "par_adjust_alike",
            &(settings(&format!(
                "{adjust}\n        rel products = [</Render/a>, </Render/b>]"
            )) + &product("a", "")
                + &product("b", PAR_2)),
        );
        assert_eq!(products.request.products.len(), 2);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A later product with another pixel aspect is written with the first's
    /// framing, like one with another policy.
    #[test]
    fn a_later_product_with_another_pixel_aspect_is_a_framing_mismatch() {
        let (products, warnings) = resolve(
            "par_mismatch",
            &(settings("        rel products = [</Render/a>, </Render/b>]")
                + &product("a", "")
                + &product("b", PAR_2)),
        );
        assert_eq!(products.request.products.len(), 2);
        assert_eq!(products.framing.pixel_aspect, 1.0);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, crate::WarningCode::ProductFramingMismatch);
        assert!(
            warnings[0].message.contains("pixelAspectRatio 2"),
            "{}",
            warnings[0].message
        );
    }

    #[test]
    fn data_types_parse_whole_names_only() {
        let ok = [
            ("float", (1, Precision::Float)),
            ("half", (1, Precision::Half)),
            ("int", (1, Precision::Uint)),
            ("double", (1, Precision::Float)),
            ("float3", (3, Precision::Float)),
            ("half4", (4, Precision::Half)),
            ("color3f", (3, Precision::Float)),
            ("color4h", (4, Precision::Half)),
            ("normal3f", (3, Precision::Float)),
            ("point3d", (3, Precision::Float)),
            ("vector3h", (3, Precision::Half)),
            ("texCoord2f", (2, Precision::Float)),
        ];
        for (name, want) in ok {
            assert_eq!(parse_data_type(name), Some(want), "{name}");
        }
        for name in [
            "floatgarbage",
            "float5",
            "float1",
            "float33",
            "color3",
            "color3x",
            "color2f",
            "normal4f",
            "texCoord2fz",
            "float[]",
            "string",
            "token",
            "",
        ] {
            assert_eq!(parse_data_type(name), None, "{name}");
        }
    }
}
