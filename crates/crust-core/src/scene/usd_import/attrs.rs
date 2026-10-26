//! Typed readers for authored attributes: the `crust:*` custom attributes, the
//! per-prim geometry flags (ray mask, motion), the subdivision level, and
//! schema-attribute value decoding. Every read resolves at [`eval_time`].

use crate::warning;
use glam::{Vec3, Vec3A};
use moho::openusd;
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::{Attribute, Prim};

use crate::ray::{MASK_ALL, RayMask};

use super::time::eval_time;

// -----------------------------------------------------------------------
// Per-prim geometry attributes (visibility mask, motion)
// -----------------------------------------------------------------------

/// `crust:rayMask` — which ray categories see this geometry (bit 0 camera,
/// bit 1 shadow, bit 2 indirect; default: all). E.g. `crust:rayMask = 6`
/// makes a light-blocker invisible to the camera.
pub(super) fn prim_ray_mask(prim: &Prim) -> RayMask {
    custom_i32(prim, "crust:rayMask")
        .map(|m| RayMask(m as u32))
        .unwrap_or(MASK_ALL)
}

/// RenderMan's per-light camera visibility, which published assets carry
/// (the Moana island's `islandPrman.usda` authors it on every light).
pub(super) const RI_CAMERA_VISIBILITY: &str = "primvars:ri:attributes:visibility:camera";

/// `crust:motion:translate` — a world-space translation the prim moves
/// through over the shutter interval (transform motion blur).
pub(super) fn prim_motion_translate(prim: &Prim) -> Option<Vec3> {
    custom_color3(prim, "crust:motion:translate").map(|v| Vec3::new(v.x, v.y, v.z))
}

/// The refinement level a subdivision surface (any scheme but `none`) gets
/// when neither the host nor the stage's `RenderSettings` names one: 0, so
/// by default nothing is refined — a subdivision surface renders its cage,
/// shaded smooth — and refinement is something a render setting or
/// `--subdiv-level` asks for, as Hydra's `refineLevel` is. Conservative on
/// purpose: every mesh not authoring `none` is a subdivision surface (ALab
/// and Kitchen_set take the fallback scheme; Moana authors `catmullClark` in
/// 189 of its 213 mesh files), and even level 1 costs them 4× their
/// triangles (ALab: 32 → 49 GiB peak).
pub(super) const DEFAULT_SUBDIV_LEVEL: u32 = 0;

/// Hard cap on the refinement level — each level quadruples the face count,
/// so 6 turns one quad into 4096 and is already past what any of the
/// checked-in scenes could resolve.
pub(super) const MAX_SUBDIV_LEVEL: u32 = 6;

/// The one refinement level every subdivided mesh of a load is refined to.
///
/// USD has no per-prim refinement level — Hydra treats it as a render
/// setting — so this is resolved once per load, in order: the host's
/// override (`UsdImportOptions::subdivision_level`, the CLI's
/// `--subdiv-level`), then `crust:subdivisionLevel` on the `RenderSettings`
/// prim, then [`DEFAULT_SUBDIV_LEVEL`]; clamped to [`MAX_SUBDIV_LEVEL`].
/// *Whether* a mesh is refined is its `subdivisionScheme`'s call
/// (see `mesh::mesh_source`), never this one's.
///
/// `CRUST_SUBDIV=0` forces 0 — the A/B switch that separates a subdivision
/// artifact from a material or lighting one, like `CRUST_MESH_BAKE`.
pub(super) fn resolve_subdiv_level(host: Option<u32>, authored: Option<i32>) -> u32 {
    if !crate::config().subdiv {
        return 0;
    }
    let (level, source) = match (host, authored) {
        (Some(l), _) => (i64::from(l), "the host override"),
        (None, Some(l)) => (i64::from(l), "crust:subdivisionLevel"),
        (None, None) => (i64::from(DEFAULT_SUBDIV_LEVEL), "the default"),
    };
    if level > i64::from(MAX_SUBDIV_LEVEL) {
        warning!(
            SubdivLevelClamped,
            "Subdivision level {level} from {source} clamped to {MAX_SUBDIV_LEVEL}"
        );
    } else if level < 0 {
        warning!(
            SubdivInvalidSetting,
            "Subdivision level {level} from {source} clamped to 0"
        );
    }
    level.clamp(0, i64::from(MAX_SUBDIV_LEVEL)) as u32
}

/// The ceiling on the adaptive level when neither the host nor the stage sets
/// a level. A round number: at 3 a cage already costs 64× its faces.
pub(super) const DEFAULT_ADAPTIVE_MAX_LEVEL: u32 = 3;

/// The adaptive-subdivision target, in pixels, in the same order as the level:
/// the host's override (`UsdImportOptions::subdivision_edge_length`, the CLI's
/// `--subdiv-edge-length`), then `crust:subdivisionEdgeLength` on the
/// `RenderSettings` prim. `None` means uniform subdivision. A value that is not
/// positive and finite is ignored with a warning — falling back to the stage's
/// when the host's is the bad one.
pub(super) fn resolve_subdiv_edge_length(host: Option<f32>, authored: Option<f32>) -> Option<f32> {
    let valid = |l: f32, source: &str| {
        if l.is_finite() && l > 0.0 {
            Some(l)
        } else {
            warning!(
                SubdivInvalidSetting,
                "Subdivision edge length {l} from {source} is not a positive pixel length — ignored"
            );
            None
        }
    };
    host.and_then(|l| valid(l, "the host override"))
        .or_else(|| authored.and_then(|l| valid(l, "crust:subdivisionEdgeLength")))
}

/// The ceiling on the adaptive level: the uniform level when the host or the
/// stage sets one (resolved and clamped as
/// [`resolve_subdiv_level`] does), else [`DEFAULT_ADAPTIVE_MAX_LEVEL`].
pub(super) fn resolve_adaptive_max_level(host: Option<u32>, authored: Option<i32>) -> u32 {
    if host.is_none() && authored.is_none() {
        return DEFAULT_ADAPTIVE_MAX_LEVEL;
    }
    resolve_subdiv_level(host, authored)
}

// -----------------------------------------------------------------------
// Value reads and decoders
// -----------------------------------------------------------------------
//
// Every attribute read in the importer goes through [`value_at`] and one of
// the `decode_*` functions below, so that two readers of the same kind of
// value cannot disagree about which authored types they accept.

/// `attr`'s value at [`eval_time`], or `None` when it is unauthored, blocked
/// or unreadable.
///
/// openusd resolves an unauthored schema attribute to its schema fallback,
/// and the importer's readers take `None` as "unauthored": a product inherits
/// the settings' resolution and a camera's focus distance is crust's 10, not
/// the schema's 0. So a
/// value equal to the fallback is asked whether a layer authored it — only
/// then, since a value that differs from the fallback was authored, and an
/// attribute no schema declares (`crust:*`, primvars, shader inputs) has none.
pub(super) fn value_at(attr: &Attribute) -> Option<sdf::Value> {
    let value = attr.get_at::<sdf::Value>(eval_time()).ok().flatten()?;
    match attr.fallback_value() {
        Ok(Some(fallback)) if fallback == value => attr.has_authored_value().ok()?.then_some(value),
        _ => Some(value),
    }
}

/// The value of `prim`'s attribute `name` at [`eval_time`] — [`value_at`] by
/// name.
pub(super) fn prim_value(prim: &Prim, name: &str) -> Option<sdf::Value> {
    value_at(&prim.attribute(name))
}

/// A scalar float, whatever precision it was authored in.
pub(super) fn decode_f32(v: sdf::Value) -> Option<f32> {
    match v {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        sdf::Value::Half(h) => Some(h.to_f32()),
        _ => None,
    }
}

/// A scalar float in double precision, whatever precision it was authored in.
pub(super) fn decode_f64(v: sdf::Value) -> Option<f64> {
    match v {
        sdf::Value::Double(d) => Some(d),
        sdf::Value::Float(f) => Some(f64::from(f)),
        sdf::Value::Half(h) => Some(f64::from(h.to_f32())),
        _ => None,
    }
}

/// A number where hand-written files author integers as often as floats
/// (`clearValue`, MaterialX-style shader inputs): [`decode_f32`] plus `int`.
pub(super) fn decode_number(v: sdf::Value) -> Option<f32> {
    match v {
        sdf::Value::Int(i) => Some(i as f32),
        v => decode_f32(v),
    }
}

pub(super) fn decode_i32(v: sdf::Value) -> Option<i32> {
    match v {
        sdf::Value::Int(i) => Some(i),
        _ => None,
    }
}

pub(super) fn decode_bool(v: sdf::Value) -> Option<bool> {
    match v {
        sdf::Value::Bool(b) => Some(b),
        // Authoring tools sometimes write bools as ints.
        sdf::Value::Int(i) => Some(i != 0),
        _ => None,
    }
}

/// A token or a string — exporters write either for the same attribute.
pub(super) fn decode_text(v: sdf::Value) -> Option<String> {
    match v {
        sdf::Value::Token(t) => Some(t.as_str().to_owned()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}

/// A three-vector (`color3f`, `float3`, `vector3d`, …) in any precision.
/// USD has no dedicated colour variant: `color3f` is a `Vec3f`.
pub(super) fn decode_vec3(v: sdf::Value) -> Option<Vec3A> {
    match v {
        sdf::Value::Vec3f(c) => Some(Vec3A::new(c.x, c.y, c.z)),
        sdf::Value::Vec3d(c) => Some(Vec3A::new(c.x as f32, c.y as f32, c.z as f32)),
        sdf::Value::Vec3h(c) => Some(Vec3A::new(c.x.to_f32(), c.y.to_f32(), c.z.to_f32())),
        _ => None,
    }
}

/// A shading value widened to four channels, the shape `UsdUVTexture`'s
/// `scale`/`bias`/`fallback` have: a `float4` as authored, a colour with
/// alpha 1, a scalar in every channel.
pub(super) fn decode_float4(v: sdf::Value) -> Option<[f32; 4]> {
    match v {
        sdf::Value::Vec4f(v) => Some([v.x, v.y, v.z, v.w]),
        sdf::Value::Vec4d(v) => Some([v.x as f32, v.y as f32, v.z as f32, v.w as f32]),
        sdf::Value::Vec4h(v) => Some([v.x.to_f32(), v.y.to_f32(), v.z.to_f32(), v.w.to_f32()]),
        v @ (sdf::Value::Vec3f(_) | sdf::Value::Vec3d(_) | sdf::Value::Vec3h(_)) => {
            decode_vec3(v).map(|c| c.extend(1.0).to_array())
        }
        v => decode_f32(v).map(|f| [f; 4]),
    }
}

pub(super) fn decode_f32_array(v: sdf::Value) -> Option<Vec<f32>> {
    match v {
        sdf::Value::FloatVec(v) => Some(v),
        sdf::Value::DoubleVec(v) => Some(v.into_iter().map(|d| d as f32).collect()),
        sdf::Value::HalfVec(v) => Some(v.into_iter().map(|h| h.to_f32()).collect()),
        _ => None,
    }
}

/// A float array in double precision, whatever precision it was authored in
/// (`knots`, `pointWeights`).
pub(super) fn decode_f64_array(v: sdf::Value) -> Option<Vec<f64>> {
    match v {
        sdf::Value::DoubleVec(v) => Some(v),
        sdf::Value::FloatVec(v) => Some(v.into_iter().map(f64::from).collect()),
        sdf::Value::HalfVec(v) => Some(v.into_iter().map(|h| f64::from(h.to_f32())).collect()),
        _ => None,
    }
}

pub(super) fn decode_i32_array(v: sdf::Value) -> Option<Vec<i32>> {
    match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    }
}

pub(super) fn decode_i64_array(v: sdf::Value) -> Option<Vec<i64>> {
    match v {
        sdf::Value::Int64Vec(v) => Some(v),
        _ => None,
    }
}

/// A `point3f[]` / `float3[]` array as authored.
pub(super) fn decode_vec3f_array(v: sdf::Value) -> Option<Vec<Vec3f>> {
    match v {
        sdf::Value::Vec3fVec(v) => Some(v),
        _ => None,
    }
}

pub(super) fn custom_i32(prim: &Prim, name: &str) -> Option<i32> {
    prim_value(prim, name).and_then(decode_i32)
}

pub(super) fn custom_token(prim: &Prim, name: &str) -> Option<String> {
    prim_value(prim, name).and_then(decode_text)
}

pub(super) fn custom_color3(prim: &Prim, name: &str) -> Option<Vec3A> {
    prim_value(prim, name).and_then(decode_vec3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdivision_edge_length_precedence_and_refusal() {
        assert_eq!(resolve_subdiv_edge_length(None, None), None, "uniform");
        assert_eq!(resolve_subdiv_edge_length(None, Some(4.0)), Some(4.0));
        assert_eq!(
            resolve_subdiv_edge_length(Some(2.0), Some(4.0)),
            Some(2.0),
            "the host wins"
        );
        assert_eq!(resolve_subdiv_edge_length(None, Some(0.0)), None);
        assert_eq!(resolve_subdiv_edge_length(None, Some(-1.0)), None);
        assert_eq!(resolve_subdiv_edge_length(None, Some(f32::INFINITY)), None);
        assert_eq!(
            resolve_subdiv_edge_length(Some(f32::NAN), Some(4.0)),
            Some(4.0),
            "a bad host value falls back to the stage's"
        );
    }

    #[test]
    fn the_adaptive_ceiling_is_the_level_setting_or_three() {
        if !crate::config().subdiv {
            return; // `CRUST_SUBDIV=0` forces every level to 0
        }
        assert_eq!(
            resolve_adaptive_max_level(None, None),
            DEFAULT_ADAPTIVE_MAX_LEVEL
        );
        assert_eq!(resolve_adaptive_max_level(Some(2), None), 2);
        assert_eq!(resolve_adaptive_max_level(None, Some(1)), 1);
        assert_eq!(
            resolve_adaptive_max_level(Some(0), Some(5)),
            0,
            "the host wins"
        );
        assert_eq!(resolve_adaptive_max_level(None, Some(40)), MAX_SUBDIV_LEVEL);
    }

    #[test]
    fn subdivision_level_precedence_and_clamp() {
        // Only meaningful with subdivision on (`CRUST_SUBDIV` unset).
        if !crate::config().subdiv {
            return;
        }
        assert_eq!(resolve_subdiv_level(None, None), DEFAULT_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(None, Some(1)), 1, "the stage's level");
        assert_eq!(resolve_subdiv_level(Some(0), Some(3)), 0, "the host wins");
        assert_eq!(resolve_subdiv_level(Some(4), None), 4);
        assert_eq!(resolve_subdiv_level(None, Some(40)), MAX_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(Some(u32::MAX), None), MAX_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(None, Some(-3)), 0);
    }
}
