use crate::ray::Ray;
use glam::{Vec3A, Vec4};
use utils::concentric_disk;

/// The camera's frame, for the camera-space sources, and its image plane,
/// for the screen-space ones. Camera space is the USD camera's: `u` right,
/// `v` up, looking down `−w`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CameraFrame {
    pub(crate) origin: Vec3A,
    pub(crate) u: Vec3A,
    pub(crate) v: Vec3A,
    pub(crate) w: Vec3A,
    /// The image plane `Camera::get_ray` aims at: its lower-left corner
    /// relative to `origin`, and its two extents (`lower_left_corner −
    /// origin`, `horizontal`, `vertical`). `horizontal` is along `u`,
    /// `vertical` along `v`. The window need not be centred on `−w`: an
    /// aperture offset moves it ([`Camera::with_window_shift`]).
    pub(crate) lower_left: Vec3A,
    pub(crate) horizontal: Vec3A,
    pub(crate) vertical: Vec3A,
    /// The image resolution, in pixels.
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl CameraFrame {
    /// `d`, a world-space offset or direction, in camera space.
    pub(crate) fn eye(&self, d: Vec3A) -> Vec3A {
        Vec3A::new(d.dot(self.u), d.dot(self.v), d.dot(self.w))
    }

    /// Camera-space depth of `p`: positive in front of the camera.
    pub(crate) fn depth(&self, p: Vec3A) -> f32 {
        -(p - self.origin).dot(self.w)
    }

    /// The pinhole inverse of `Camera::get_ray` at the lens centre: the
    /// viewport coordinates `(s, t)` whose ray passes through `p` — in
    /// `[0, 1]²` on the frame, outside it off the frame, `t` growing along
    /// `v` (up). Depth of field is ignored on purpose: the vector built on
    /// this describes the in-focus image. `p` must be in front of the camera
    /// (depth > 0); callers clip first.
    pub(crate) fn proj(&self, p: Vec3A) -> (f32, f32) {
        let d = p - self.origin;
        // The plane lies `f` in front of the origin, and the ray through
        // `(s, t)` lands on it at `lower_left + s·horizontal + t·vertical`,
        // so scaling `d` onto the plane by depth gives that point.
        let f = -self.lower_left.dot(self.w);
        let on_plane = d * (f / self.depth(p));
        let rel = on_plane - self.lower_left;
        (
            rel.dot(self.u) / self.horizontal.length(),
            rel.dot(self.v) / self.vertical.length(),
        )
    }
}

/// The `Camera` struct represents a virtual camera in the ray tracing system.
/// It is responsible for generating rays that simulate the perspective view of a scene.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// The origin of the camera (position in 3D space).
    origin: Vec3A,
    /// The lower-left corner of the viewport.
    lower_left_corner: Vec3A,
    /// The horizontal vector of the viewport.
    horizontal: Vec3A,
    /// The vertical vector of the viewport.
    vertical: Vec3A,
    /// The camera's local horizontal axis.
    u: Vec3A,
    /// The camera's local vertical axis.
    v: Vec3A,
    /// The radius of the camera's lens (used for depth of field).
    lens_radius: f32,
    /// `UsdGeomCamera.clippingRange` and `clippingPlanes`, as authored.
    clip: CameraClip,
}

/// The most `clippingPlanes` a camera honours: what OpenGL guarantees
/// (`GL_MAX_CLIP_PLANES`), so a stage that renders in Storm's viewport fits.
/// A fixed bound keeps [`Camera`] `Copy`; the import warns about the rest.
pub const MAX_CLIP_PLANES: usize = 8;

/// What the camera clips its own segment to, as world-space planes `(n, d)`
/// each keeping `n·p + d ≥ 0`, precomputed by [`Camera::with_clipping`].
///
/// Every test is on a point the camera ray met, never on the ray: the
/// tracer applies them to every hit of the camera segment
/// (`tracer::path::skip_clipped`), so they hold however that segment was
/// restarted past cutouts or medium boundaries, and the camera ray itself is
/// built exactly as it is without clipping — a near distance that moved its
/// origin instead ran `implicit_shapes` 0.2% more instructions through every
/// camera, clipping or not. Secondary rays are never clipped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CameraClip {
    /// The near distance (camera-space depth `≥ near`); `None`: none.
    near: Option<Vec4>,
    /// The far distance (depth `≤ far`); `None`: none.
    far: Option<Vec4>,
    /// The kept half-spaces of `clippingPlanes`.
    planes: [Vec4; MAX_CLIP_PLANES],
    count: u8,
}

impl CameraClip {
    const NONE: CameraClip = CameraClip {
        near: None,
        far: None,
        planes: [Vec4::ZERO; MAX_CLIP_PLANES],
        count: 0,
    };

    /// Whether a point the camera ray met is beyond the far distance: every
    /// later point along that ray is too, so the segment ends.
    #[inline]
    pub(crate) fn beyond_far(&self, p: Vec3A) -> bool {
        self.far.is_some_and(|f| f.dot(p.extend(1.0)) < 0.0)
    }

    /// Whether a point the camera ray met is nearer than the near distance
    /// or outside a kept half-space of `clippingPlanes`: the ray goes on
    /// past it.
    #[inline]
    pub(crate) fn clipped(&self, p: Vec3A) -> bool {
        let p = p.extend(1.0);
        self.near.is_some_and(|n| n.dot(p) < 0.0)
            || self.planes[..self.count as usize]
                .iter()
                .any(|plane| plane.dot(p) < 0.0)
    }

    /// Whether it clips anything at all.
    fn clips(&self) -> bool {
        self.near.is_some() || self.far.is_some() || self.count > 0
    }
}

impl Camera {
    /// This camera with its lens closed: every ray leaves the eye, as
    /// `disableDepthOfField` asks of the procedural fallback too.
    pub(crate) fn pinhole(self) -> Self {
        Self {
            lens_radius: 0.0,
            ..self
        }
    }

    /// Creates a new `Camera` with the specified parameters.
    pub fn new(
        lookfrom: Vec3A,
        lookat: Vec3A,
        vup: Vec3A,
        vfov: f32, // Vertical field-of-view in degrees
        aspect_ratio: f32,
        aperture: f32,
        focus_dist: f32,
    ) -> Camera {
        let theta = utils::degrees_to_radians(vfov);
        let h = f32::tan(theta / 2.0);
        let viewport_height = 2.0 * h;
        let viewport_width = aspect_ratio * viewport_height;
        let w = (lookfrom - lookat).normalize();
        let u = vup.cross(w).normalize();
        let v = w.cross(u);

        let origin = lookfrom;
        let horizontal = focus_dist * viewport_width * u;
        let vertical = focus_dist * viewport_height * v;
        let lower_left_corner = origin - horizontal / 2.0 - vertical / 2.0 - focus_dist * w;

        let lens_radius = aperture / 2.0;

        Camera {
            origin,
            lower_left_corner,
            horizontal,
            vertical,
            u,
            v,
            lens_radius,
            clip: CameraClip::NONE,
        }
    }

    /// This camera clipped as `UsdGeomCamera` clips: the camera sees only
    /// what lies `near` to `far` along its view axis (camera-space depth,
    /// so the near clip is a plane, not a sphere) and inside every one of
    /// `planes`. Each plane `(a, b, c, d)` is in camera space — `u` right,
    /// `v` up, `w` backward, from the eye — and keeps `a·x + b·y + c·z + d ≥
    /// 0`. A `near` of 0 and an infinite `far` clip nothing; planes past
    /// [`MAX_CLIP_PLANES`] are dropped (the importer warns). The caller
    /// validates: `0 ≤ near < far`, finite planes.
    #[must_use = "returns a new camera; the original is unchanged"]
    pub fn with_clipping(self, near: f32, far: f32, planes: &[[f32; 4]]) -> Camera {
        let w = self.u.cross(self.v);
        // A camera-space plane `(n_c, d)` at world point `p`: the camera-space
        // point is `((p − o)·u, (p − o)·v, (p − o)·w)`, so the world plane is
        // `n = a·u + b·v + c·w`, `d' = d − n·o`.
        let to_world = |[a, b, c, d]: [f32; 4]| {
            let n = a * self.u + b * self.v + c * w;
            n.extend(d - n.dot(self.origin))
        };
        let mut clip = CameraClip {
            // Depth is `−z`: `depth ≥ near` is the plane `−z − near ≥ 0`,
            // `depth ≤ far` the plane `z + far ≥ 0`.
            near: (near > 0.0).then(|| to_world([0.0, 0.0, -1.0, -near])),
            far: far.is_finite().then(|| to_world([0.0, 0.0, 1.0, far])),
            ..CameraClip::NONE
        };
        for (slot, plane) in clip.planes.iter_mut().zip(planes) {
            *slot = to_world(*plane);
            clip.count += 1;
        }
        Camera { clip, ..self }
    }

    /// What the tracer tests the camera segment's hits against; `None` when
    /// the camera does not clip, the usual case.
    pub(crate) fn hit_clip(&self) -> Option<&CameraClip> {
        self.clip.clips().then_some(&self.clip)
    }

    /// This camera with its image window moved off the optical axis by
    /// `shift`, in frames: `[1, 0]` moves it one frame width right (along
    /// `u`), `[0, 1]` one frame height up (along `v`). A film-back offset
    /// over its aperture (`UsdGeomCamera`'s `horizontalApertureOffset /
    /// horizontalAperture`); the eye and the focus plane stay where they
    /// are, so verticals stay vertical. No shift leaves the camera bit for
    /// bit.
    pub fn with_window_shift(self, shift: [f32; 2]) -> Self {
        if shift == [0.0, 0.0] {
            return self;
        }
        Self {
            lower_left_corner: self.lower_left_corner
                + shift[0] * self.horizontal
                + shift[1] * self.vertical,
            ..self
        }
    }

    /// Whether the lens is open: rays leave a disk rather than the eye, and
    /// out-of-focus depths blur.
    pub fn has_depth_of_field(&self) -> bool {
        self.lens_radius > 0.0
    }

    /// The world-space width one pixel spans at ray parameter `t = 1`.
    ///
    /// This is the numerator of a primary ray's cone spread. `get_ray` builds
    /// its direction as `lower_left_corner + s·horizontal + t·vertical -
    /// origin`, and `lower_left_corner` is offset from the origin by
    /// `focus_dist · w` — so the direction lands *on the focus plane* at ray
    /// parameter 1, and one pixel of `s` moves that landing point by exactly
    /// `horizontal / res_w`. The two axes are averaged because the cone is
    /// isotropic; a non-square pixel is filtered by the mean of its sides.
    ///
    /// `focus_dist` cancels: `|horizontal| = focus_dist · viewport_width` and
    /// the direction's length at frame centre is `focus_dist`, so dividing by
    /// it leaves `2·tan(vfov/2)/res_h` — the classic per-pixel angle, which is
    /// what the unit test pins. The division is the caller's, per ray, because
    /// the direction is longer towards the frame edge and that pixel really
    /// does subtend a smaller angle.
    /// The camera position at shutter open.
    pub(crate) fn origin(&self) -> Vec3A {
        self.origin
    }

    /// The camera's frame at shutter open, for the camera-space AOVs —
    /// right, up, and backward (the view direction is `−w`, as in a USD
    /// camera); `get_ray` builds `v = w × u`, so `w = u × v` — and its image
    /// plane and resolution, for the screen-space ones.
    pub(crate) fn frame(&self, width: usize, height: usize) -> CameraFrame {
        CameraFrame {
            origin: self.origin,
            u: self.u,
            v: self.v,
            w: self.u.cross(self.v),
            lower_left: self.lower_left_corner - self.origin,
            horizontal: self.horizontal,
            vertical: self.vertical,
            width: width as f32,
            height: height as f32,
        }
    }

    pub fn pixel_span(&self, res_w: usize, res_h: usize) -> f32 {
        0.5 * (self.horizontal.length() / res_w.max(1) as f32
            + self.vertical.length() / res_h.max(1) as f32)
    }

    /// Generates a ray originating from the camera through the viewport.
    ///
    /// # Parameters
    /// - `s`, `t`: Normalized viewport coordinates in `[0, 1]`.
    /// - `lens_uv`: A 2D uniform sample used to sample the lens for depth of
    ///   field. Ignored when the camera has zero aperture. Callers pass a QMC
    ///   sample so this dimension is decorrelated from the pixel jitter.
    /// - `time`: Shutter time in `[0, 1)`, carried on the ray for motion
    ///   blur (moving instances interpolate their transform at this time).
    pub fn get_ray(&self, s: f32, t: f32, lens_uv: [f32; 2], time: f32) -> Ray {
        let offset = if self.lens_radius > 0.0 {
            let rd = self.lens_radius * concentric_disk(lens_uv);
            self.u * rd.x + self.v * rd.y
        } else {
            Vec3A::ZERO
        };
        Ray::new(
            self.origin + offset,
            self.lower_left_corner + s * self.horizontal + t * self.vertical - self.origin - offset,
        )
        .with_time(time)
        .with_mask(crate::ray::MASK_CAMERA)
    }
}
