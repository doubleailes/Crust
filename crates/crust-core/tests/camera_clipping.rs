//! `UsdGeomCamera.clippingRange` and `clippingPlanes` in the integrator: the
//! camera segment starts on the near distance, ends at the far one, and
//! passes every hit outside a clipping plane, while every other ray meets the
//! clipped geometry as the geometry it is. Built on hand-assembled worlds;
//! the import side is at the end, on small stages.

use crust_core::rt::Geometry;
use crust_core::{
    AovFilm, AovProduct, AovRequest, AovSource, AovVar, AreaLight, Buffer, Camera, Emissive,
    LightList, OpenPBR, PixelFilter, Precision, RenderSettings, Renderer, Scene, SphereShape,
    Vec3A, WarningCode, World, WorldBuilder,
};
use std::path::PathBuf;
use std::sync::Arc;

/// A quad in the plane z = `z`, over `x0..x1` × `-h..h`, facing +Z.
fn wall(z: f32, x0: f32, x1: f32, h: f32) -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![[x0, -h, z], [x1, -h, z], [x1, h, z], [x0, h, z]],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        normals: None,
    }
}

/// A black surface emitting `color`: what the camera sees of it is its
/// emission alone.
fn glowing(color: Vec3A) -> Arc<OpenPBR> {
    Arc::new(OpenPBR {
        emission_luminance: 1.0,
        emission_color: color,
        ..OpenPBR::diffuse(Vec3A::ZERO)
    })
}

const RED: Vec3A = Vec3A::new(1.0, 0.0, 0.0);
const GREEN: Vec3A = Vec3A::new(0.0, 1.0, 0.0);

/// A red emitter at depth 2 in front of a green one at depth 10, both
/// filling the view; with `cutout`, a fully transparent cutout card at
/// depth 1 in front of both.
fn two_walls(cutout: bool) -> World {
    let mut b = WorldBuilder::new();
    b.attach(wall(-10.0, -100.0, 100.0, 100.0), glowing(GREEN));
    b.attach(wall(-2.0, -100.0, 100.0, 100.0), glowing(RED));
    if cutout {
        b.attach(
            wall(-1.0, -100.0, 100.0, 100.0),
            Arc::new(OpenPBR {
                geometry_opacity: 0.0,
                ..OpenPBR::diffuse(Vec3A::ONE)
            }),
        );
    }
    b.commit()
}

/// A camera at the origin looking down −Z, `w`×`h` pixels, box-filtered,
/// `spp` samples per pixel on every pixel.
fn renderer(world: World, lights: LightList, camera: Camera, w: usize, h: usize) -> Renderer {
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(4)
        .with_adaptive_sampling(4, 0.0)
        .with_max_depth(4)
        .with_indirect_clamp(0.0)
        .with_pixel_filter("box".parse::<PixelFilter>().unwrap());
    Renderer::new(camera, world, lights, settings)
}

fn camera(w: usize, h: usize) -> Camera {
    Camera::new(
        Vec3A::ZERO,
        -Vec3A::Z,
        Vec3A::Y,
        40.0,
        w as f32 / h as f32,
        0.0,
        1.0,
    )
}

fn every_pixel(beauty: &Buffer, w: usize, h: usize, expected: Vec3A) {
    for y in 0..h {
        for x in 0..w {
            let p = beauty.get_pixel(x, y);
            assert!(
                p.abs_diff_eq(expected, 1e-5),
                "({x}, {y}): {p}, not {expected}"
            );
        }
    }
}

#[test]
fn an_unclipped_camera_sees_the_nearest_wall() {
    let r = renderer(two_walls(false), LightList::new(), camera(6, 4), 6, 4);
    every_pixel(&r.render(), 6, 4, RED);
}

#[test]
fn the_near_distance_sees_through_a_nearer_wall() {
    let cam = camera(6, 4).with_clipping(5.0, f32::INFINITY, &[]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, GREEN);
}

/// An orthographic camera clips by the same depths, measured from its own
/// image plane (the camera's position), which its parallel rays leave from.
#[test]
fn an_orthographic_camera_clips_by_the_same_depths() {
    let ortho = Camera::orthographic(Vec3A::ZERO, -Vec3A::Z, Vec3A::Y, 6.0, 4.0);
    let r = renderer(two_walls(false), LightList::new(), ortho, 6, 4);
    every_pixel(&r.render(), 6, 4, RED);
    let near = ortho.with_clipping(5.0, f32::INFINITY, &[]);
    let r = renderer(two_walls(false), LightList::new(), near, 6, 4);
    every_pixel(&r.render(), 6, 4, GREEN);
    // Far in front of both walls: nothing is seen.
    let far = ortho.with_clipping(0.0, 1.5, &[]);
    let r = renderer(two_walls(false), LightList::new(), far, 6, 4);
    every_pixel(&r.render(), 6, 4, Vec3A::ZERO);
}

#[test]
fn the_far_distance_ends_the_camera_ray() {
    // Far between the walls: the red one stays.
    let cam = camera(6, 4).with_clipping(0.0, 5.0, &[]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, RED);
    // Both walls outside the range: the camera ray escapes.
    let cam = camera(6, 4).with_clipping(3.0, 8.0, &[]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, Vec3A::ZERO);
}

#[test]
fn a_clipping_plane_keeps_its_half_space() {
    // Camera space `x ≥ 0`: the left half of the frame sees nothing.
    let cam = camera(6, 4).with_clipping(0.0, f32::INFINITY, &[[1.0, 0.0, 0.0, 0.0]]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    let beauty = r.render();
    for y in 0..4 {
        for x in 0..6 {
            let expected = if x < 3 { Vec3A::ZERO } else { RED };
            let p = beauty.get_pixel(x, y);
            assert!(p.abs_diff_eq(expected, 1e-5), "({x}, {y}): {p}");
        }
    }
    // Camera space `z ≤ −5` (depth ≥ 5): the ray continues past the red wall.
    let cam = camera(6, 4).with_clipping(0.0, f32::INFINITY, &[[0.0, 0.0, -1.0, -5.0]]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, GREEN);
}

/// A surface met after passing a cutout is clipped as the first hit is.
#[test]
fn a_surface_behind_a_cutout_is_clipped_too() {
    let cam = camera(6, 4).with_clipping(0.0, f32::INFINITY, &[[0.0, 0.0, -1.0, -5.0]]);
    let r = renderer(two_walls(true), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, GREEN);
    let cam = camera(6, 4).with_clipping(0.0, 5.0, &[]);
    let r = renderer(two_walls(true), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, RED);
}

fn depth_var() -> AovVar {
    AovVar {
        prim_path: "/Render/Vars/depth".into(),
        name: "depth".into(),
        channel_prefix: None,
        source: AovSource::Depth,
        components: AovSource::Depth.components(),
        precision: Precision::Float,
        accumulation: AovSource::Depth.default_accumulation(),
        clear: AovSource::Depth.default_clear(),
        expression: None,
        raw: false,
        variance: false,
    }
}

fn depth(r: &Renderer) -> Vec<f32> {
    let var = depth_var();
    let request = AovRequest {
        precomp: None,
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars: vec![var.clone()],
            attributes: Vec::new(),
        }],
    };
    let (beauty, film, _): (Buffer, AovFilm, _) = r.render_with_aovs(true, &|_, _| {}, &request);
    film.var_channels(&beauty, &var).remove(0)
}

#[test]
fn the_depth_aov_reports_the_first_unclipped_hit() {
    let near = renderer(
        two_walls(false),
        LightList::new(),
        camera(6, 4).with_clipping(5.0, f32::INFINITY, &[]),
        6,
        4,
    );
    for d in depth(&near) {
        assert!((d - 10.0).abs() < 1e-3, "{d}");
    }
    let plane = renderer(
        two_walls(false),
        LightList::new(),
        camera(6, 4).with_clipping(0.0, f32::INFINITY, &[[0.0, 0.0, -1.0, -5.0]]),
        6,
        4,
    );
    for d in depth(&plane) {
        assert!((d - 10.0).abs() < 1e-3, "{d}");
    }
}

/// Clipping is the camera's alone: a blocker the camera does not see still
/// casts its shadow.
#[test]
fn a_clipped_surface_still_shadows() {
    let mut b = WorldBuilder::new();
    let grey = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
    b.attach(wall(-10.0, -100.0, 100.0, 100.0), grey.clone());
    // Covers the left of the light's view of the back wall.
    b.attach(wall(-4.0, -100.0, 0.0, 100.0), grey);
    let mut lights = LightList::new();
    let emitter = Arc::new(Emissive::light(Vec3A::splat(40.0), None));
    let shape = SphereShape {
        center: Vec3A::new(0.0, 0.0, -2.0),
        radius: 0.2,
    };
    let id = b.attach_masked(
        Geometry::Sphere {
            center: shape.center,
            radius: shape.radius,
        },
        emitter.clone(),
        crust_core::MASK_INDIRECT,
    );
    b.set_transparent_emitter(id, true);
    lights.add(AreaLight::new(shape, emitter, id));
    let cam = camera(8, 2).with_clipping(5.0, f32::INFINITY, &[]);
    let beauty = renderer(b.commit(), lights, cam, 8, 2).render();
    let (left, right) = (beauty.get_pixel(0, 0), beauty.get_pixel(7, 0));
    assert!(right.x > 1e-4, "the right of the back wall is lit: {right}");
    assert!(
        left.x < right.x * 1e-3,
        "the left is in the blocker's shadow: {left}"
    );
}

/// The near distance is a depth along the view axis, not a distance from the
/// eye: at the frame's corners the red wall at depth 2 is more than 2.1 away
/// from the eye, and is clipped all the same.
#[test]
fn the_near_distance_is_measured_along_the_view_axis() {
    let cam = camera(6, 4).with_clipping(2.1, f32::INFINITY, &[]);
    let r = renderer(two_walls(false), LightList::new(), cam, 6, 4);
    every_pixel(&r.render(), 6, 4, GREEN);
}

// ---- import ----

fn write_stage(name: &str, camera_body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_clipping_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(
        &path,
        format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "Cam"
    {{
{camera_body}
        float focalLength = 500
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Sphere "Ball" {{ double radius = 2 }}
}}
"#
        ),
    )
    .unwrap();
    path
}

fn load(name: &str, camera_body: &str) -> Scene {
    Scene::from_usd(&write_stage(name, camera_body)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The `depth` AOV at the centre pixel of a 3×3 render of the stage. The
/// ball's front is 3 in front of the camera; from inside, its back is 7. The
/// long lens keeps the pixel on the axis whatever its jitter, so the depth
/// does not follow the sphere's curve.
fn centre_depth(scene: Scene) -> f32 {
    let settings = RenderSettings::default()
        .with_resolution(3, 3)
        .with_samples_per_pixel(1)
        .with_adaptive_sampling(1, 0.0)
        .with_max_depth(1)
        .with_indirect_clamp(0.0);
    let r = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    depth(&r)[4]
}

#[test]
fn an_authored_clipping_range_clips_the_render() {
    let scene = load("range", "        float2 clippingRange = (4, 1000)");
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    let d = centre_depth(scene);
    assert!((d - 7.0).abs() < 0.2, "the ball's back: {d}");
    let scene = load("range_far", "        float2 clippingRange = (1, 2)");
    let d = centre_depth(scene);
    assert!(d.is_infinite(), "the ball is past the far distance: {d}");
}

/// An unauthored range clips nothing, rather than at USD's fallback near of 1.
#[test]
fn an_unauthored_clipping_range_clips_nothing() {
    let d = centre_depth(load("unauthored", ""));
    assert!((d - 3.0).abs() < 0.2, "{d}");
}

#[test]
fn an_invalid_clipping_range_is_refused() {
    let scene = load("invalid", "        float2 clippingRange = (10, 2)");
    assert!(
        scene
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::CameraInvalidClipping),
        "{:?}",
        scene.warnings
    );
    let d = centre_depth(scene);
    assert!((d - 3.0).abs() < 0.2, "{d}");
}

#[test]
fn clipping_planes_past_the_eighth_are_dropped_with_a_warning() {
    let planes = ["(0, 0, -1, -1)"; 9].join(", ");
    let scene = load(
        "planes",
        &format!("        float4[] clippingPlanes = [{planes}]"),
    );
    assert!(
        scene
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::CameraTooManyClippingPlanes),
        "{:?}",
        scene.warnings
    );
}

/// The ball at the origin, its front 3 in front of the camera and its back
/// 7, wholly nearer than a plane that keeps depth ≥ 8: the centre pixel sees
/// past it.
#[test]
fn authored_clipping_planes_clip_the_render() {
    let clipped = centre_depth(load(
        "plane",
        "        float4[] clippingPlanes = [(0, 0, -1, -8)]",
    ));
    assert!(clipped.is_infinite(), "the ball is clipped: {clipped}");
}
