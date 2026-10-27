//! The public resolved view reproduces the actual primary rays.
use crust_core::Camera;
use glam::Vec3A;

#[test]
fn resolved_perspective_and_orthographic_views_match_their_rays() {
    let eye = Vec3A::new(3.0, 4.0, 8.0);
    for camera in [
        Camera::new(eye, Vec3A::ZERO, Vec3A::Y, 45.0, 1.5, 0.0, 8.0),
        Camera::orthographic(eye, Vec3A::ZERO, Vec3A::Y, 6.0, 4.0),
    ] {
        let camera = camera
            .with_window_shift([0.15, -0.2])
            .with_clipping(0.25, 400.0, &[]);
        let view = camera.resolved_view();
        let v = |i: usize| {
            Vec3A::new(
                view.camera_to_world[i][0] as f32,
                view.camera_to_world[i][1] as f32,
                view.camera_to_world[i][2] as f32,
            )
        };
        let [l, r, b, t] = view.window.map(|v| v as f32);
        for (s, t_coord) in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (0.25, 0.75)] {
            let ray = camera.get_ray(s, t_coord, [0.0, 0.0], 0.0);
            let on_window = v(0) * (l + (r - l) * s) + v(1) * (b + (t - b) * t_coord);
            let expected_origin = if view.orthographic {
                v(3) + on_window
            } else {
                v(3)
            };
            let expected_direction = if view.orthographic {
                -v(2)
            } else {
                (on_window - v(2)).normalize()
            };
            assert!((ray.origin() - expected_origin).length() < 2e-5);
            assert!((ray.direction().normalize() - expected_direction).length() < 2e-5);
        }
        assert!((view.clipping[0] - 0.25).abs() < 2e-5);
        // World-space plane coefficients are f32: allow two ULPs at 400.
        assert!((view.clipping[1] - 400.0).abs() < 1e-4);
    }
}
