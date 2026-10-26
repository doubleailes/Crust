//! `crust:volume:*` prims → [`VolumeRegion`]s.

use crate::warning;
use glam::{Mat4 as GMat4, Vec3A};
use tracing::debug;

use crate::color::Space;
use crate::volume::{DensityField, VolumeRegion};

use super::index::data::PrimData;

/// Import a `crust:volume:*` prim as a `VolumeRegion`. The local box is
/// `[-size/2, size/2]^3` when the prim authors a `size` attribute (a
/// `Cube`'s convention; USD's default cube size is 2) and the unit cube
/// `[-0.5, 0.5]^3` otherwise; placement, orientation and scale come from
/// the composed prim transform.
pub(super) fn emit_volume(
    prim: &PrimData,
    world_xf: GMat4,
    volumes: &mut Vec<VolumeRegion>,
    working: Space,
) {
    let ty = prim
        .custom_token("crust:volume:type")
        .expect("checked by dispatch");

    let field = match ty.as_str() {
        "homogeneous" => DensityField::Homogeneous,
        "smoke" => DensityField::Noise {
            scale: prim.custom_f32("crust:volume:noiseScale").unwrap_or(4.0),
            octaves: prim
                .custom_i32("crust:volume:noiseOctaves")
                .unwrap_or(4)
                .max(1) as u32,
            gain: prim.custom_f32("crust:volume:noiseGain").unwrap_or(0.5),
            lacunarity: prim
                .custom_f32("crust:volume:noiseLacunarity")
                .unwrap_or(2.0),
            threshold: prim
                .custom_f32("crust:volume:noiseThreshold")
                .unwrap_or(0.3),
            seed: prim.custom_i32("crust:volume:noiseSeed").unwrap_or(0) as u32,
        },
        "grid" => {
            let dims = prim.custom_i32_array("crust:volume:gridDims");
            let data = prim.custom_f32_array("crust:volume:gridData");
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
                        "Volume at {}: grid type needs int[3] crust:volume:gridDims and float[] crust:volume:gridData — skipped",
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
                "Volume at {}: unknown crust:volume:type \"{}\" (expected homogeneous | smoke | grid) — skipped",
                prim.path(),
                other
            );
            return;
        }
    };

    // Per-channel coefficients, so they are colours in the working space
    // like any other: converted only when `colorSpace` names another.
    let sigma_s = prim
        .custom_color("crust:volume:sigmaS", working)
        .unwrap_or(Vec3A::splat(0.5));
    let sigma_a = prim
        .custom_color("crust:volume:sigmaA", working)
        .unwrap_or(Vec3A::ZERO);
    let emission = prim
        .custom_color("crust:volume:emission", working)
        .unwrap_or(Vec3A::ZERO);
    let g = prim.custom_f32("crust:volume:anisotropy").unwrap_or(0.0);
    let density_scale = prim.custom_f32("crust:volume:densityScale").unwrap_or(1.0);
    let half = prim.custom_f32("size").map_or(0.5, |s| s * 0.5);

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
