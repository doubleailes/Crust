//! Prims carrying `primvars:crust:volume:*` → [`VolumeRegion`]s. Crust's
//! volume attributes are constant primvars, as Hydra carries them (design
//! D5).

use crate::warning;
use glam::{Mat4 as GMat4, Vec3A};
use tracing::debug;

use crate::color::Space;
use crate::volume::{DensityField, VolumeRegion};

use super::attrs::{decode_f32, decode_f32_array, decode_i32_array, decode_text, decode_vec3};
use super::index::data::PrimData;
use super::index::primvars::Primvar;

/// Import a `primvars:crust:volume:*` prim as a `VolumeRegion`. The local box
/// is `[-size/2, size/2]^3` for a `Cube` (its Hydra `cube/size`; USD's
/// default cube size is 2) and the unit cube `[-0.5, 0.5]^3` otherwise;
/// placement, orientation and scale come from the prim's Hydra transform.
pub(super) fn emit_volume(
    prim: &PrimData,
    world_xf: GMat4,
    volumes: &mut Vec<VolumeRegion>,
    working: Space,
) {
    let Some(ty) = volume_type(prim) else {
        return;
    };

    let field = match ty.as_str() {
        "homogeneous" => DensityField::Homogeneous,
        "smoke" => DensityField::Noise {
            scale: vf32(prim, "noiseScale").unwrap_or(4.0),
            octaves: vi32(prim, "noiseOctaves").unwrap_or(4).max(1) as u32,
            gain: vf32(prim, "noiseGain").unwrap_or(0.5),
            lacunarity: vf32(prim, "noiseLacunarity").unwrap_or(2.0),
            threshold: vf32(prim, "noiseThreshold").unwrap_or(0.3),
            seed: vi32(prim, "noiseSeed").unwrap_or(0) as u32,
        },
        "grid" => {
            let dims = volume_primvar(prim, "gridDims").and_then(|p| decode_i32_array(p.value));
            let data = volume_primvar(prim, "gridData").and_then(|p| decode_f32_array(p.value));
            match (dims, data) {
                (Some(d), Some(data)) if d.len() == 3 => {
                    let (nx, ny, nz) = (
                        d[0].max(1) as usize,
                        d[1].max(1) as usize,
                        d[2].max(1) as usize,
                    );
                    if nx * ny * nz != data.len() {
                        warning!(
                            VolumeInvalidGrid,
                            at = prim.path(),
                            "Volume at {}: gridDims {}x{}x{} does not match gridData length {} — skipped",
                            prim.path(),
                            nx,
                            ny,
                            nz,
                            data.len()
                        );
                        return;
                    }
                    DensityField::Grid { nx, ny, nz, data }
                }
                _ => {
                    warning!(
                        VolumeInvalidGrid,
                        at = prim.path(),
                        "Volume at {}: grid type needs int[3] primvars:crust:volume:gridDims and float[] primvars:crust:volume:gridData — skipped",
                        prim.path()
                    );
                    return;
                }
            }
        }
        other => {
            warning!(
                VolumeUnknownType,
                at = prim.path(),
                "Volume at {}: unknown primvars:crust:volume:type \"{}\" (expected homogeneous | smoke | grid) — skipped",
                prim.path(),
                other
            );
            return;
        }
    };

    // Per-channel coefficients, so they are colours in the working space
    // like any other: converted only when `colorSpace` names another.
    let sigma_s = vcolor(prim, "sigmaS", working).unwrap_or(Vec3A::splat(0.5));
    let sigma_a = vcolor(prim, "sigmaA", working).unwrap_or(Vec3A::ZERO);
    let emission = vcolor(prim, "emission", working).unwrap_or(Vec3A::ZERO);
    let g = vf32(prim, "anisotropy").unwrap_or(0.0);
    let density_scale = vf32(prim, "densityScale").unwrap_or(1.0);
    let half = match prim.ty() {
        moho::tokens::prim_type::CUBE => prim
            .locate([moho::tokens::prim_type::CUBE, moho::tokens::implicit::SIZE])
            .and_then(|d| d.value())
            .and_then(decode_f32)
            .map_or(0.5, |s| s * 0.5),
        _ => 0.5,
    };

    debug!(
        "Imported {} volume at {} (densityScale={})",
        ty,
        prim.path(),
        density_scale
    );
    volumes.push(VolumeRegion::new(
        world_xf,
        Vec3A::splat(half),
        sigma_s,
        sigma_a,
        g,
        emission,
        density_scale,
        field,
    ));
}

/// The volume primvar `primvars:crust:volume:<name>` of `prim`.
fn volume_primvar(prim: &PrimData, name: &str) -> Option<Primvar> {
    prim.primvar(&format!("crust:volume:{name}"))
}

/// The prim's `primvars:crust:volume:type`, which makes it a volume region
/// (checked before any other dispatch), `None` when it carries none.
pub(super) fn volume_type(prim: &PrimData) -> Option<String> {
    volume_primvar(prim, "type").and_then(|p| decode_text(p.value))
}

fn vf32(prim: &PrimData, name: &str) -> Option<f32> {
    volume_primvar(prim, name).and_then(|p| p.f32())
}

fn vi32(prim: &PrimData, name: &str) -> Option<i32> {
    volume_primvar(prim, name).and_then(|p| p.i32())
}

/// A per-channel coefficient, in the working space: converted from the
/// colour space the primvar names, as authored when it names none.
fn vcolor(prim: &PrimData, name: &str, working: Space) -> Option<Vec3A> {
    let p = volume_primvar(prim, name)?;
    let rgb = decode_vec3(p.value.clone())?;
    let space = p.color_space.as_deref().and_then(|n| {
        super::materials::named_space(
            &prim.attribute_path(&format!("primvars:crust:volume:{name}")),
            n,
        )
    });
    Some(match space {
        Some(space) => crate::color::convert(rgb, space, working),
        None => rgb,
    })
}
