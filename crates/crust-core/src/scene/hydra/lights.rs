//! UsdLux lights → [`LightList`] entries (and their emissive scene geometry),
//! read from the scene index: each light's parameters from its Hydra `light`
//! data, its USD type and applied API schemas from `__usdPrimInfo`. The
//! light's transform is the commit's.

use crate::{record_warning, warning};
use std::sync::Arc;

use crust_rt::Geometry;
use glam::{Affine3A, Mat3A, Mat4 as GMat4, Vec3, Vec3A};
use tracing::debug;

use crate::color::Space;
use crate::light::{
    AffineShape, AreaLight, AreaShape, DistantLight as CoreDistantLight,
    DomeLight as CoreDomeLight, LightList, LightShape, LobeGains, RectShape, SphereShape,
    UnitShape,
};
use crate::lux::{IesShaping, Shaping, distant_illuminance, distant_size_factor};
use crate::material::Emissive;
use crate::rt_world::WorldBuilder;

use super::assets::{asset_value_path, cached_asset, explained, timed_asset};
use super::attrs::{RI_CAMERA_VISIBILITY, decode_text, decode_vec3};
use super::index::data::{PrimData, up_axis};
use super::index::fields::Fields;
use super::materials::named_space;
use super::{ImportCaches, ImportCtx};

/// The `LightAPI` quantities every UsdLux light shares.
struct LuxParams {
    /// `L_Color` in the spec's notation, in nits:
    /// `intensity · 2^exposure · color`, times the colour temperature's
    /// blackbody when `enableColorTemperature` is on. Before `normalize`.
    pub(in crate::scene) emission: Vec3A,
    /// `inputs:normalize` — divide by the light's size (see each light).
    pub(in crate::scene) normalize: bool,
    /// `inputs:diffuse` / `inputs:specular`, the per-lobe multipliers.
    pub(in crate::scene) gains: LobeGains,
}

/// The `LightAPI` inputs as authored — before any colour space conversion,
/// colour temperature or transform — with each unauthored or non-finite one
/// at its schema fallback: what [`lux_params`] computes the emission from,
/// and what `crust ls light --json` reports.
pub(in crate::scene) struct LightInputs {
    pub(in crate::scene) intensity: f32,
    pub(in crate::scene) exposure: f32,
    pub(in crate::scene) color: Vec3A,
    pub(in crate::scene) normalize: bool,
    /// `inputs:diffuse` / `inputs:specular`, the per-lobe multipliers. Any
    /// finite value, negative included (as the colour may be).
    pub(in crate::scene) gains: LobeGains,
}

/// Reads [`LightInputs`], warning about a non-finite value.
pub(in crate::scene) fn light_inputs(prim: &PrimData) -> LightInputs {
    // A non-finite value would reach both MIS halves as NaN radiance, so it
    // falls back to the schema default like the shaping inputs do.
    let finite = |name: &str, v: Option<f32>, fallback: f32| match v {
        Some(x) if !x.is_finite() => {
            warning!(
                LightNonFiniteInput,
                at = prim.path(),
                "{}: inputs:{name} = {x} is not finite — using its fallback {fallback}",
                prim.path()
            );
            fallback
        }
        v => v.unwrap_or(fallback),
    };
    let intensity = finite("intensity", lp(prim).f32("intensity"), 1.0);
    let exposure = finite("exposure", lp(prim).f32("exposure"), 0.0);
    let color = match lp(prim).value("color").and_then(decode_vec3) {
        Some(c) if !c.is_finite() => {
            warning!(
                LightNonFiniteInput,
                at = prim.path(),
                "{}: inputs:color = {:?} is not finite — using its fallback (1, 1, 1)",
                prim.path(),
                c.to_array()
            );
            Vec3A::ONE
        }
        c => c.unwrap_or(Vec3A::ONE),
    };
    LightInputs {
        intensity,
        exposure,
        color,
        normalize: lp(prim).bool("normalize").unwrap_or(false),
        gains: LobeGains {
            diffuse: finite("diffuse", lp(prim).f32("diffuse"), 1.0),
            specular: finite("specular", lp(prim).f32("specular"), 1.0),
        },
    }
}

/// Reads the shared `LightAPI` inputs.
fn lux_params(prim: &PrimData, working: Space) -> LuxParams {
    let LightInputs {
        intensity,
        exposure,
        color,
        normalize,
        gains,
    } = light_inputs(prim);
    let color = match param_color_space(prim, "color") {
        Some(space) => crate::color::convert(color, space, working),
        None => color,
    };
    let gain = intensity * 2f32.powf(exposure);
    let mut emission = color * gain;

    if lp(prim).bool("enableColorTemperature").unwrap_or(false) {
        let kelvin = lp(prim).f32("colorTemperature").unwrap_or(6500.0);
        // The blackbody leaves the Rec.709 gamut below ~1900 K; clamp the
        // product, not the factor, so a negative channel cannot flip sign
        // against a negative `color`.
        // Straight from the locus into the working space, normalised to
        // unit luminance there.
        let tint = crate::lux::blackbody_in(kelvin, working);
        emission = (emission * tint).max(Vec3A::ZERO);
    }

    LuxParams {
        emission,
        normalize,
        gains,
    }
}

/// The light's `ShapingAPI`, or `None` when nothing about it shapes.
///
/// Read off the prim directly rather than through `ShapingAPI::get`, since
/// shaping attributes authored without the API applied are common in
/// exported stages and hdEmbree honours them. What the API's presence
/// changes is the *fallback* of an unauthored input: with `ShapingAPI`
/// applied the schema's `cone:angle = 90` is in force (Hydra hands the
/// delegate the schema fallback), without it there is no such attribute and
/// the cone is open (hdEmbree's own default, 180°). openusd does not report
/// schema fallbacks for applied API schemas, so that rule is applied here;
/// whether the API is applied is `__usdPrimInfo/apiSchemas`.
fn lux_shaping(
    prim: &PrimData,
    light_to_world: Mat3A,
    caches: &mut ImportCaches,
) -> Option<Shaping> {
    let applied = prim.api_schemas().iter().any(|a| a == "ShapingAPI");
    // A non-finite authored value survives every formula below and turns the
    // light's radiance — both MIS halves of it — into NaN. Such a value is
    // refused and replaced by the input's fallback, with a warning.
    let finite = |name: &str, v: Option<f32>| match v {
        Some(x) if !x.is_finite() => {
            warning!(
                LightNonFiniteInput,
                at = prim.path(),
                "{}: {name} = {x} is not finite — using its fallback",
                prim.path()
            );
            None
        }
        v => v,
    };
    let mut shaping = Shaping::new(light_to_world);
    shaping.focus = finite("inputs:shaping:focus", lp(prim).f32("shaping:focus")).unwrap_or(0.0);
    // The schema's fallback is black, not white ("The default tint is
    // black", usdLux/schema.usda): unauthored, focus darkens off-axis
    // emission rather than leaving it neutral, exactly as in hdEmbree.
    let focus_tint = lp(prim).value("shaping:focusTint").and_then(decode_vec3);
    let focus_tint = focus_tint.map(|rgb| match param_color_space(prim, "shaping:focusTint") {
        Some(space) => crate::color::convert(rgb, space, caches.working),
        None => rgb,
    });
    shaping.focus_tint = match focus_tint {
        Some(c) if !c.is_finite() => {
            warning!(
                LightNonFiniteInput,
                at = prim.path(),
                "{}: inputs:shaping:focusTint = {c} is not finite — using its fallback",
                prim.path()
            );
            Vec3A::ZERO
        }
        c => c.unwrap_or(Vec3A::ZERO),
    };
    shaping.cone_angle_deg = finite(
        "inputs:shaping:cone:angle",
        lp(prim).f32("shaping:cone:angle"),
    )
    .unwrap_or(if applied {
        Shaping::SCHEMA_CONE_ANGLE_DEG
    } else {
        180.0
    });
    shaping.cone_softness = finite(
        "inputs:shaping:cone:softness",
        lp(prim).f32("shaping:cone:softness"),
    )
    .unwrap_or(0.0);

    let ies_file = lp(prim).value("shaping:ies:file").and_then(|v| {
        asset_value_path(&v, caches.stage_path, &|| {
            prim.path().append_property("inputs:shaping:ies:file").ok()
        })
    });
    if let Some(path) = ies_file {
        let assets = caches.assets;
        let profile = cached_asset(
            &mut caches.ies,
            &mut caches.asset_time,
            path.clone(),
            |path| explained(&mut caches.failed_assets, path, || assets.load_ies(path)),
        );
        // Every light referencing a profile the host could not read counts,
        // a cache hit included; the host logged the cause once per file.
        if profile.is_none() && caches.failed_assets.contains(&path) {
            record_warning!(
                IesUnreadable,
                at = prim.path(),
                "{}: could not load IES profile {} — the light renders without it",
                prim.path(),
                path.display()
            );
        }
        shaping.ies = profile.map(|profile| IesShaping {
            profile,
            angle_scale: finite(
                "inputs:shaping:ies:angleScale",
                lp(prim).f32("shaping:ies:angleScale"),
            )
            .unwrap_or(0.0),
            normalize: lp(prim).bool("shaping:ies:normalize").unwrap_or(false),
        });
    }

    (!shaping.is_neutral()).then_some(shaping)
}

/// The linear part of a matrix, for direction-only uses.
fn linear_part(m: GMat4) -> Mat3A {
    Mat3A::from_mat4(m)
}

/// Uniform scale of `m` along the named local axes, if it is a similarity
/// there — equal lengths, mutually perpendicular — and snapped to exactly 1
/// when within rounding of it, so an unscaled light keeps its authored
/// radius to the bit.
pub(in crate::scene) fn similarity_scale(m: Mat3A, axes: &[usize]) -> Option<f32> {
    let cols = [m.x_axis, m.y_axis, m.z_axis];
    let s = cols[axes[0]].length();
    if s.is_nan() || s <= 0.0 {
        return None;
    }
    if axes
        .iter()
        .any(|&a| (cols[a].length() - s).abs() > 1e-5 * s)
    {
        return None;
    }
    for (i, &a) in axes.iter().enumerate() {
        if axes[i + 1..]
            .iter()
            .any(|&b| !perpendicular(cols[a], cols[b]))
        {
            return None;
        }
    }
    Some(if (s - 1.0).abs() < 1e-6 { 1.0 } else { s })
}

fn perpendicular(a: Vec3A, b: Vec3A) -> bool {
    a.dot(b).abs() <= 1e-5 * a.length() * b.length()
}

/// Attaches a sphere, disk or cylinder light — the three UsdLux lights
/// defined on a round [`UnitShape`] — of the given `radius` (and, for the
/// cylinder, `length`) under the prim transform `world_xf`.
///
/// Where `world_xf` is a similarity over the axes the shape is round in, the
/// geometry is the kernel's world-space analytic primitive, as a sphere light
/// always was, with the authored radius times that scale. Otherwise — a
/// non-uniform scale squashing it into an ellipsoid, an ellipse or an
/// elliptical tube — it is the unit primitive placed by an instance, which
/// the kernel intersects exactly under any affine map. Either way the light
/// samples the same [`AffineShape`] (or, for the round sphere, the historical
/// [`SphereShape`], so existing scenes render exactly as before), and
/// `normalize` divides by its world-space area.
#[allow(clippy::too_many_arguments)]
fn emit_round_light(
    world: &mut WorldBuilder,
    lights: &mut LightList,
    prim: &PrimData,
    unit: UnitShape,
    world_xf: GMat4,
    radius: f32,
    length: f32,
    params: LuxParams,
    shaping: Option<Shaping>,
) {
    // A negative or zero radius (or length) would pass the affine check —
    // a negative scale is a reflection, still invertible — while the kernel
    // refuses the matching primitive, leaving a light NEE samples on a
    // surface no ray can hit. Refuse it here, once, for both.
    let valid = |x: f32| x.is_finite() && x > 0.0;
    if !valid(radius) || (unit == UnitShape::Cylinder && !valid(length)) {
        warning!(
            LightDegenerateShape,
            at = prim.path(),
            "{}: {:?} light radius {radius}{} must be finite and positive — skipped",
            prim.path(),
            unit,
            if unit == UnitShape::Cylinder {
                format!(", length {length}")
            } else {
                String::new()
            }
        );
        return;
    }
    let local = match unit {
        UnitShape::Sphere => Vec3::splat(radius),
        UnitShape::Disk => Vec3::new(radius, radius, 1.0),
        UnitShape::Cylinder => Vec3::new(length, radius, radius),
    };
    let l2w = Affine3A::from_mat4(world_xf * GMat4::from_scale(local));
    let Some(affine) = AffineShape::new(unit, l2w) else {
        warning!(
            LightDegenerateShape,
            at = prim.path(),
            "{}: light transform collapses its shape (zero size or scale) — skipped",
            prim.path()
        );
        return;
    };
    let area = affine.area();
    let radiance = if params.normalize {
        params.emission / area.max(1e-30)
    } else {
        params.emission
    };
    let material = Arc::new(Emissive::light(radiance, shaping));
    let m = linear_part(world_xf);
    let origin = l2w.translation;

    let direct = match unit {
        UnitShape::Sphere => similarity_scale(m, &[0, 1, 2]).map(|s| Geometry::Sphere {
            center: origin,
            radius: radius * s,
        }),
        UnitShape::Disk => similarity_scale(m, &[0, 1]).map(|s| Geometry::Disk {
            center: origin,
            // Local −Z under the normal transform: the emitting side, which
            // the kernel reports as the disk's front.
            normal: m.inverse().transpose() * -Vec3A::Z,
            radius: radius * s,
        }),
        UnitShape::Cylinder => similarity_scale(m, &[1, 2])
            .filter(|_| perpendicular(m.x_axis, m.y_axis) && perpendicular(m.x_axis, m.z_axis))
            .map(|s| Geometry::Cylinder {
                p0: l2w.transform_point3a(Vec3A::new(-0.5, 0.0, 0.0)),
                p1: l2w.transform_point3a(Vec3A::new(0.5, 0.0, 0.0)),
                radius: radius * s,
            }),
    };
    let analytic = direct.is_some();
    let round_sphere = match &direct {
        Some(Geometry::Sphere { center, radius }) => Some(SphereShape {
            center: *center,
            radius: *radius,
        }),
        _ => None,
    };
    let geometry = direct.unwrap_or_else(|| {
        let mut b = crust_rt::SceneBuilder::new();
        b.attach(match unit {
            UnitShape::Sphere => Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            UnitShape::Disk => Geometry::Disk {
                center: Vec3A::ZERO,
                normal: -Vec3A::Z,
                radius: 1.0,
            },
            UnitShape::Cylinder => Geometry::Cylinder {
                p0: Vec3A::new(-0.5, 0.0, 0.0),
                p1: Vec3A::new(0.5, 0.0, 0.0),
                radius: 1.0,
            },
        });
        Geometry::Instance {
            scene: Arc::new(b.commit_with(crate::commit_options())),
            transform: l2w,
            transform_end: None,
        }
    });
    let (mask, transparent) = light_ray_mask(prim);
    let geom_id = world.attach_masked(geometry, material.clone(), mask);
    world.set_transparent_emitter(geom_id, transparent);

    let shape: AreaShape = match round_sphere {
        Some(sphere) => sphere.into(),
        None => affine.into(),
    };
    lights.add(AreaLight::new(shape, material, geom_id));
    tag_last(lights, prim, params.gains);
    debug!(
        "{:?} light {}: area={} normalize={} radiance={:?} ({})",
        unit,
        prim.path(),
        area,
        params.normalize,
        radiance,
        if analytic { "analytic" } else { "instanced" }
    );
}

/// `UsdLuxSphereLight`: radius `inputs:radius` (0.5), scaled by the prim
/// transform — a non-uniform scale makes an ellipsoid. `treatAsPoint` is a
/// hint for renderers without area lights and is ignored, as the schema
/// permits.
pub(in crate::scene) fn emit_sphere_light(ctx: &mut ImportCtx, prim: &PrimData, world_xf: GMat4) {
    let radius = lp(prim).f32("radius").unwrap_or(0.5);
    let params = lux_params(prim, ctx.caches.working);
    let shaping = lux_shaping(prim, linear_part(world_xf), &mut ctx.caches);
    emit_round_light(
        &mut ctx.world,
        &mut ctx.lights,
        prim,
        UnitShape::Sphere,
        world_xf,
        radius,
        0.0,
        params,
        shaping,
    );
}

/// `UsdLuxDiskLight`: a disk of `inputs:radius` (0.5) in the local XY
/// plane, emitting from one side, along local −Z.
pub(in crate::scene) fn emit_disk_light(ctx: &mut ImportCtx, prim: &PrimData, world_xf: GMat4) {
    let radius = lp(prim).f32("radius").unwrap_or(0.5);
    let params = lux_params(prim, ctx.caches.working);
    let shaping = lux_shaping(prim, linear_part(world_xf), &mut ctx.caches);
    emit_round_light(
        &mut ctx.world,
        &mut ctx.lights,
        prim,
        UnitShape::Disk,
        world_xf,
        radius,
        0.0,
        params,
        shaping,
    );
}

/// `UsdLuxCylinderLight`: a tube of `inputs:radius` (0.5) and
/// `inputs:length` (1) along local X, centred on the origin, emitting
/// outward from its side and not from its end caps. `treatAsLine` is
/// ignored, as `treatAsPoint` is.
pub(in crate::scene) fn emit_cylinder_light(ctx: &mut ImportCtx, prim: &PrimData, world_xf: GMat4) {
    let radius = lp(prim).f32("radius").unwrap_or(0.5);
    let length = lp(prim).f32("length").unwrap_or(1.0);
    let params = lux_params(prim, ctx.caches.working);
    let shaping = lux_shaping(prim, linear_part(world_xf), &mut ctx.caches);
    emit_round_light(
        &mut ctx.world,
        &mut ctx.lights,
        prim,
        UnitShape::Cylinder,
        world_xf,
        radius,
        length,
        params,
        shaping,
    );
}

/// The colour space a light's image file is decoded from: the space its own
/// `colorSpace` metadatum names (not a scope's `colorSpace:name`, which
/// describes colour values — see `attr_own_color_space`), else `auto` — sRGB
/// for an 8-bit image, the working space for a float one.
///
/// A light's image is radiance, never data, so a `raw` tag means "used as
/// stored" — already in the working space — and keeps that space: the
/// environment's importance sampling weighs its texels by the working
/// space's luminance, which `ColorSpace::RAW` would lose.
fn texture_color_space(prim: &PrimData, working: Space) -> crate::ColorSpace {
    let own = param_color_space(prim, "texture:file");
    match own {
        Some(space) if space.is_data() => crate::ColorSpace::new(working, working),
        Some(space) => crate::ColorSpace::new(space, working),
        None => crate::ColorSpace::AUTO.into_working(working),
    }
}

/// `RectLight`'s `inputs:texture:file`, decoded by the host. Cached by
/// resolved path: a rig commonly reuses one card texture on many lights.
fn rect_light_texture(
    prim: &PrimData,
    caches: &mut ImportCaches,
) -> Option<Arc<crate::LightTexture>> {
    let file = "inputs:texture:file";
    let path = lp(prim).value("texture:file").and_then(|v| {
        asset_value_path(&v, caches.stage_path, &|| {
            prim.path().append_property(file).ok()
        })
    })?;
    let space = texture_color_space(prim, caches.working);
    let assets = caches.assets;
    let loaded = cached_asset(
        &mut caches.light_textures,
        &mut caches.asset_time,
        (path.clone(), space),
        |(path, space)| {
            let loaded = explained(&mut caches.failed_assets, path, || {
                assets.load_light_texture(path, *space)
            });
            if let Some(t) = &loaded {
                debug!(
                    "RectLight {}: texture {} ({}x{})",
                    prim.path(),
                    path.display(),
                    t.width(),
                    t.height()
                );
            }
            loaded
        },
    );
    // Counted per light, a cache hit included; the host logged the cause.
    if loaded.is_none() && caches.failed_assets.contains(&path) {
        record_warning!(
            LightMapUnreadable,
            at = prim.path(),
            "RectLight at {}: could not load inputs:texture:file {} — the light emits its \
             uniform colour",
            prim.path(),
            path.display()
        );
    }
    loaded
}

/// `UsdLuxRectLight`: a `width × height` rectangle (1 × 1) in the local XY
/// plane, centred on the origin, emitting from one side, along local −Z, and
/// multiplied by `inputs:texture:file` when one is authored (image top row
/// at the light's +Y edge, left column at −X).
pub(in crate::scene) fn emit_rect_light(ctx: &mut ImportCtx, prim: &PrimData, world_xf: GMat4) {
    let width = lp(prim).f32("width").unwrap_or(1.0);
    let height = lp(prim).f32("height").unwrap_or(1.0);
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        warning!(
            LightDegenerateShape,
            at = prim.path(),
            "RectLight at {}: width {width} × height {height} must be finite and \
             positive — skipped",
            prim.path()
        );
        return;
    }
    let params = lux_params(prim, ctx.caches.working);
    let shaping = lux_shaping(prim, linear_part(world_xf), &mut ctx.caches);
    let texture = rect_light_texture(prim, &mut ctx.caches);

    let corner = world_xf.transform_point3(Vec3::new(-0.5 * width, -0.5 * height, 0.0));
    let origin = Vec3A::new(corner.x, corner.y, corner.z);
    let eu = world_xf.transform_vector3(Vec3::new(width, 0.0, 0.0));
    let ev = world_xf.transform_vector3(Vec3::new(0.0, height, 0.0));
    let edge_u = Vec3A::new(eu.x, eu.y, eu.z);
    let edge_v = Vec3A::new(ev.x, ev.y, ev.z);

    // The emitting normal is local −Z under the *normal* transform, which is
    // ±(edge_u × edge_v): the plain transformed −Z is only perpendicular to
    // the rectangle while the transform is a similarity, and under a
    // sheared or non-uniformly scaled rotation it is not. The transformed
    // axis is kept when it is (every ordinary light), so those render as
    // they always have.
    let nz = world_xf.transform_vector3(Vec3::NEG_Z);
    let nz = Vec3A::new(nz.x, nz.y, nz.z);
    let cross = edge_u.cross(edge_v);
    let det = linear_part(world_xf).determinant();
    let exact = -cross * det.signum();
    let normal = if nz.dot(edge_u).abs() <= 1e-5 * nz.length() * edge_u.length()
        && nz.dot(edge_v).abs() <= 1e-5 * nz.length() * edge_v.length()
    {
        nz
    } else {
        exact
    };

    let area = cross.length();
    let radiance = if params.normalize {
        params.emission / area.max(1e-30)
    } else {
        params.emission
    };

    // The geometry (one mesh: two triangles spanning the rectangle) and the
    // AreaLight share one surface; bounce hits are attributed to the light by
    // the geometry id. The triangles are wound so their geometric normal is
    // the *emitting* one, because a light's surface is one-sided and
    // `front_face` is how a bounce hit knows which side it arrived on.
    let mut emitter = Emissive::light(radiance, shaping);
    if let Some(image) = texture
        && let Some(map) = crate::RectTexture::new(image, origin, edge_u, edge_v)
    {
        emitter = emitter.with_texture(map);
    }
    let material = Arc::new(emitter);
    let (c00, c10, c11, c01) = (
        origin,
        origin + edge_u,
        origin + edge_u + edge_v,
        origin + edge_v,
    );
    let indices = if cross.dot(normal) > 0.0 {
        vec![[0, 1, 2], [0, 2, 3]]
    } else {
        vec![[0, 2, 1], [0, 3, 2]]
    };
    let (mask, transparent) = light_ray_mask(prim);
    let geom_id = ctx.world.attach_masked(
        Geometry::TriangleMesh {
            vertices: vec![
                c00.to_array(),
                c10.to_array(),
                c11.to_array(),
                c01.to_array(),
            ],
            indices,
            normals: None,
        },
        material.clone(),
        mask,
    );
    ctx.world.set_transparent_emitter(geom_id, transparent);
    ctx.lights.add(AreaLight::new(
        RectShape::new(origin, edge_u, edge_v, normal),
        material,
        geom_id,
    ));
    tag_last(&mut ctx.lights, prim, params.gains);
    debug!(
        "RectLight: origin={:?} edge_u={:?} edge_v={:?} normalize={} radiance={:?}",
        origin, edge_u, edge_v, params.normalize, radiance
    );
}

/// Imports a `UsdLuxDistantLight`. The light points down its local -Z, so
/// the world direction it travels toward is that axis under the prim's
/// transform. `inputs:angle` is the source's angular *diameter* in degrees
/// (default 0.53 — the sun's).
///
/// Units follow the spec. `intensity × color × 2^exposure` is the source's
/// **luminance** in nits; with `inputs:normalize` it is divided by
/// [`distant_size_factor`] (`π·sin²θmax`), which makes it the
/// **illuminance** in lux on a surface facing the light. A zero angle is a
/// delta light, whose `intensity` both the spec (`sizeFactor = 1`) and
/// hdEmbree deliver as that illuminance.
///
/// crust widens a cone narrower than `MIN_DISTANT_ANGLE_DEG` (see
/// [`DistantLight`](crate::light::DistantLight)) rather than carrying a
/// delta light, and the widening preserves the *illuminance* the
/// authored cone would have delivered — so it moves the penumbra and nothing
/// else. The light has no scene geometry, so it is light-list-only: bounce
/// rays find it by escaping along a direction inside its cone.
pub(in crate::scene) fn emit_distant_light(
    lights: &mut LightList,
    prim: &PrimData,
    world_xf: GMat4,
    working: Space,
) {
    let direction = world_xf.transform_vector3(Vec3::NEG_Z);
    if direction.length_squared() < 1e-12 {
        warning!(
            LightDegenerateShape,
            at = prim.path(),
            "DistantLight has a degenerate orientation — skipped"
        );
        return;
    }
    let angle = lp(prim).f32("angle").unwrap_or(0.53).max(0.0);
    let params = lux_params(prim, working);
    let direction = Vec3A::new(direction.x, direction.y, direction.z);

    // Everything becomes the illuminance the *authored* cone delivers to a
    // facing surface, computed in f64, and `DistantLight::new` spreads it
    // over the cone crust actually samples. That one quantity covers all
    // four cases exactly: nits (`L·π·sin²θ`), lux (`normalize`), a delta
    // light (its intensity *is* lux), and a cone narrower than the widening
    // floor — or so narrow its f32 cosine rounds to 1 — whose illuminance
    // the widened cone keeps.
    let illuminance = if angle == 0.0 {
        params.emission
    } else {
        let per_nit = distant_illuminance(1.0, angle);
        let luminance = if params.normalize {
            params.emission / distant_size_factor(angle)
        } else {
            params.emission
        };
        luminance * per_nit
    };
    let light = CoreDistantLight::new(direction, illuminance, angle);
    let mask = infinite_light_escape_mask(prim);
    debug!(
        "DistantLight {}: direction={:?} angle={}° normalize={} camera-visible={}",
        prim.path(),
        direction,
        angle,
        params.normalize,
        mask.sees(crate::ray::MASK_CAMERA)
    );
    lights.add_masked(light, mask);
    tag_last(lights, prim, params.gains);
}

/// What [`emit_dome_light`] reads from either dome schema. `DomeLight_1`
/// does not derive from `DomeLight`, so each answers for its own
/// declarations; both name the same inputs.
pub(in crate::scene) struct DomeInputs {
    schema: &'static str,
    /// Turns the dome's own frame onto its pole axis, before the prim's
    /// transform: the identity, or +90° about X for a +Z pole.
    pole: Mat3A,
}

impl DomeInputs {
    /// The dome `prim` is, by its USD type: `None` for a prim that is no dome.
    ///
    /// A `UsdLuxDomeLight`'s pole is +Y whatever the stage's up axis. Its
    /// schema declares no `poleAxis`, and an authored one is ignored, as
    /// OpenUSD's imaging reads it only through `UsdLuxDomeLight_1`.
    ///
    /// A `UsdLuxDomeLight_1`'s `poleAxis` puts its top pole on +Z when it is
    /// `Z`, or `scene` (the fallback) on a stage whose `upAxis` is `Z`. That
    /// is OpenUSD's `_GetDomeOffset` (`usdImaging/domeLight_1Adapter.cpp`): a
    /// +90° rotation about X, for the dome alone. Typhoon (hdEmbree) never
    /// applies it; crust follows the schema.
    pub(in crate::scene) fn of(prim: &PrimData) -> Option<Self> {
        if prim.is_a("DomeLight") {
            return Some(Self {
                schema: "DomeLight",
                pole: Mat3A::IDENTITY,
            });
        }
        match prim.is_a("DomeLight_1") {
            true => {
                let z_pole = match lp(prim).text("poleAxis").as_deref() {
                    Some("Z") => true,
                    Some("Y") => false,
                    // `scene`, the fallback: the stage's `upAxis`, whose
                    // own fallback is Y.
                    _ => prim
                        .index()
                        .and_then(|index| up_axis(index.as_ref()))
                        .is_some_and(|axis| axis == "Z"),
                };
                Some(Self {
                    schema: "DomeLight_1",
                    // Columns are the images of local X, Y, Z: +Y → +Z,
                    // +Z → −Y.
                    pole: if z_pole {
                        Mat3A::from_cols(Vec3A::X, Vec3A::Z, -Vec3A::Y)
                    } else {
                        Mat3A::IDENTITY
                    },
                })
            }
            false => None,
        }
    }
}

/// Imports a `UsdLuxDomeLight` or `UsdLuxDomeLight_1` as an infinite
/// environment.
///
/// `inputs:texture:file` is resolved against the USD layer's directory and
/// handed to the host's [`AssetLoader`] — crust-core decodes nothing
/// itself. Without a file, or when the host declines, the dome is its
/// uniform `intensity × color × 2^exposure` (× the colour temperature's
/// blackbody, when enabled).
///
/// Only `latlong` is supported; `inputs:texture:format` values that mean
/// anything else warn and fall back to the uniform colour rather than
/// silently mapping the image wrongly. The pole rotation, then the prim's
/// rotation, orient the sky.
pub(in crate::scene) fn emit_dome_light(
    ctx: &mut ImportCtx,
    prim: &PrimData,
    dome: &DomeInputs,
    world_xf: GMat4,
) {
    // The authoring layer anchors the path, not the root layer: the Moana
    // island's lights author `../textures/islandsun.exr` relative to
    // `usd/island.usda`, an `asset` the index resolves on that layer (see
    // `asset_value_path`).
    let file = "inputs:texture:file";
    let texture_path = lp(prim).value("texture:file").and_then(|v| {
        asset_value_path(&v, ctx.stage_path, &|| {
            prim.path().append_property(file).ok()
        })
    });
    let (lights, assets) = (&mut ctx.lights, ctx.assets);
    let working = ctx.caches.working;
    // Accumulates time spent in the host's decoder, so the report can
    // separate "decoding a 14k HDRI" from the rest of the traversal.
    let asset_time = &mut ctx.caches.asset_time;
    // `normalize` does not apply to a dome (its sizeFactor is 1).
    let LuxParams {
        emission: tint,
        gains,
        ..
    } = lux_params(prim, working);

    let format = lp(prim).value("texture:format").and_then(decode_text);
    let map = match texture_path {
        Some(texture) => match format.as_deref() {
            // `automatic` infers from the image; for the equirectangular
            // images a dome light normally carries that means latlong.
            None | Some("latlong") | Some("automatic") => {
                let space = texture_color_space(prim, working);
                let failed = &mut ctx.caches.failed_assets;
                let kept = ctx.caches.environments;
                let loaded = match kept.and_then(|k| k.get(&texture, space)) {
                    Some(map) => Some(map),
                    None => {
                        // Stamped before the decode: a file rewritten while
                        // it decodes is not kept under its new stamp.
                        let before = kept.and_then(|_| stamp(&texture));
                        let map = timed_asset(asset_time, || {
                            explained(failed, &texture, || {
                                assets.load_environment(&texture, space)
                            })
                        })
                        .map(Arc::new);
                        if let (Some(kept), Some(map)) = (kept, &map) {
                            kept.put(&texture, space, map.clone(), before);
                        }
                        map
                    }
                };
                if loaded.is_none() && failed.contains(&texture) {
                    record_warning!(
                        LightMapUnreadable,
                        at = prim.path(),
                        "{} at {}: could not load {} — falling back to \
                         the uniform colour",
                        dome.schema,
                        prim.path(),
                        texture.display()
                    );
                }
                loaded
            }
            Some(other) => {
                warning!(
                    LightUnsupportedTextureFormat,
                    at = prim.path(),
                    "{} at {}: texture:format \"{other}\" is not supported \
                     (only latlong) — falling back to the uniform colour",
                    dome.schema,
                    prim.path()
                );
                None
            }
        },
        None => None,
    };

    // Only the rotation orients the sky; a dome is at infinity, so its
    // translation and scale are meaningless. The pole turns the dome's own
    // frame first, so it never reaches the prim's namespace children.
    let m = world_xf.to_cols_array_2d();
    let rotation = Mat3A::from_cols(
        Vec3A::new(m[0][0], m[0][1], m[0][2]).normalize_or(Vec3A::X),
        Vec3A::new(m[1][0], m[1][1], m[1][2]).normalize_or(Vec3A::Y),
        Vec3A::new(m[2][0], m[2][1], m[2][2]).normalize_or(Vec3A::Z),
    ) * dome.pole;

    let mask = infinite_light_escape_mask(prim);
    debug!(
        "Imported {} at {} (tint={:?}, {}, camera-visible={})",
        dome.schema,
        prim.path(),
        tint,
        match &map {
            Some(m) => format!("{}x{} environment map", m.width(), m.height()),
            None => "uniform".to_string(),
        },
        mask.sees(crate::ray::MASK_CAMERA)
    );
    lights.add_masked(CoreDomeLight::new(tint, map, rotation), mask);
    tag_last(lights, prim, gains);
}

/// The environment maps a kept delegate's commits and light rebuilds share:
/// decoded and importance-sampled once (seconds for the Moana island's sky),
/// handed out again while the file keeps its length and modification time.
/// A light layer rebuilt in place would otherwise decode every dome again.
/// The map is a function of the file and the colour space alone, so a kept
/// one is the one a fresh decode gives.
#[derive(Default)]
pub(crate) struct Environments(std::cell::RefCell<std::collections::HashMap<EnvKey, KeptEnv>>);

/// An environment map's file and the colour space it was decoded into.
type EnvKey = (std::path::PathBuf, crate::ColorSpace);

/// A kept environment map and the file stamp it was decoded at.
type KeptEnv = (
    Option<(u64, std::time::SystemTime)>,
    Arc<crate::EnvironmentMap>,
);

/// A file's length and modification time, when it can be read.
fn stamp(path: &std::path::Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl Environments {
    fn get(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<Arc<crate::EnvironmentMap>> {
        let kept = self.0.borrow();
        let (at, map) = kept.get(&(path.to_owned(), space))?;
        (at.is_some() && *at == stamp(path)).then(|| map.clone())
    }

    /// Keeps `map`, decoded from `path` as it was at `before`: only when the
    /// file still has that stamp, so a file rewritten during the decode is
    /// decoded again next time rather than handed out stale.
    fn put(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
        map: Arc<crate::EnvironmentMap>,
        before: Option<(u64, std::time::SystemTime)>,
    ) {
        if before.is_none() || before != stamp(path) {
            return;
        }
        self.0
            .borrow_mut()
            .insert((path.to_owned(), space), (before, map));
    }

    /// Lets go of every map no light holds any more: after an update, a dome
    /// removed or retextured leaves its old map here alone.
    pub(in crate::scene) fn sweep(&self) {
        self.0
            .borrow_mut()
            .retain(|_, (_, map)| Arc::strong_count(map) > 1);
    }
}

/// Gives the light just added its name, the prim path, its per-lobe
/// multipliers, and its light-path-expression tag, `token crust:light:lpeTag`
/// — the label a light group `<L.'tag'>` selects. Other renderers' tag
/// attributes are not read yet: their exact names are still to be checked
/// against real exports.
fn tag_last(lights: &mut LightList, prim: &PrimData, gains: LobeGains) {
    let index = lights.count() - 1;
    lights.set_name(index, &prim.path().to_string());
    if gains != LobeGains::ONE {
        debug!(
            "{}: diffuse ×{}, specular ×{}",
            prim.path(),
            gains.diffuse,
            gains.specular
        );
        lights.set_lobe_gains(index, gains);
    }
    if let Some(tag) = lp(prim).text("crust:light:lpeTag") {
        debug!("{}: LPE tag {tag:?}", prim.path());
        lights.set_lpe_tag(index, Some(&tag));
    }
}

/// `crust:light:cameraVisible` first, RenderMan's primvar (an int, non-zero
/// meaning visible) as the portable fallback (`attrs::light_ray_mask`).
fn authored_camera_visibility(prim: &PrimData) -> Option<bool> {
    let light = lp(prim);
    light
        .bool("crust:light:cameraVisible")
        .or_else(|| light.bool(RI_CAMERA_VISIBILITY))
}

/// Ray mask for a light's source geometry, and whether that source is a
/// transparent emitter, from the light's `light` data.
fn light_ray_mask(prim: &PrimData) -> (crate::ray::RayMask, bool) {
    use crate::ray::{MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, RayMask};
    if let Some(m) = lp(prim).i32("crust:rayMask") {
        return (RayMask(m as u32), false);
    }
    if authored_camera_visibility(prim).unwrap_or(false) {
        (MASK_CAMERA | MASK_SHADOW | MASK_INDIRECT, false)
    } else {
        (MASK_INDIRECT, true)
    }
}

/// Which escaping rays see a light at infinity:
/// from the light's `light` data.
fn infinite_light_escape_mask(prim: &PrimData) -> crate::ray::RayMask {
    use crate::ray::{MASK_ALL, MASK_CAMERA, RayMask};
    if authored_camera_visibility(prim).unwrap_or(true) {
        MASK_ALL
    } else {
        RayMask(MASK_ALL.0 & !MASK_CAMERA.0)
    }
}

/// The light `prim`'s Hydra `light` data: Hydra's light tokens read their
/// `inputs:` attribute (`intensity`), any other name the attribute of that
/// name (`crust:rayMask`, `crust:light:*`,
/// `primvars:ri:attributes:visibility:camera`), as UsdImaging's
/// `LookupLightParamAttribute` answers it (design D5). Its schema fallbacks
/// are given as Hydra gives them.
fn lp(prim: &PrimData) -> Fields {
    Fields::of(prim, [moho::tokens::light::LIGHT])
}

/// The colour space the light `prim`'s parameter `name` (a colour, or an
/// asset) names, as Hydra carries it: the parameter's `colorSpace` in the
/// light's material network, where the light is the one node.
fn param_color_space(prim: &PrimData, name: &str) -> Option<Space> {
    let node = prim.path().to_string();
    super::index::network::networks(prim)
        .values()
        .find_map(|network| {
            network
                .nodes
                .get(&node)?
                .parameters
                .get(name)?
                .color_space
                .clone()
        })
        .filter(|n| !n.is_empty())
        .and_then(|n| named_space(&prim.attribute_path(&format!("inputs:{name}")), &n))
}
