//! Authored normals on `subdivisionScheme = "none"` meshes, end to end from
//! USD: every interpolation, indexed values, `primvars:normals` over
//! `normals`, hard edges splitting their points without disturbing per-point
//! primvars, the refusals, and subdivision meshes ignoring them.
//!
//! Every mesh is two unit quads side by side in the z = 0 plane, x in
//! [0, 2], sharing the edge x = 1; rays come straight down -z, so a hit's
//! shading normal is reported facing +z, exactly as authored.

use crust_core::{Ray, Scene, Vec3A, WarningCode};
use std::path::PathBuf;

/// A directory of its own per test, for the stage and its `.mtlx`.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("crust_mesh_normal_tests")
        .join(name);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A stage of `body` under `/World`, with `/World/Looks/M` a pure emitter of
/// the `ramp` primvar (grey where unauthored), so a hit reads back both its
/// shading normal and a per-point primvar.
fn load(name: &str, body: &str) -> Scene {
    let d = dir(name);
    let mtlx = d.join("m.mtlx");
    std::fs::write(
        &mtlx,
        r#"<?xml version="1.0"?>
<materialx version="1.39">
  <geompropvalue name="value" type="color3">
    <input name="geomprop" type="string" value="ramp" />
    <input name="default" type="color3" value="0.5, 0.5, 0.5" />
  </geompropvalue>
  <uniform_edf name="edf" type="EDF"><input name="color" type="color3" nodename="value" /></uniform_edf>
  <surface name="surf" type="surfaceshader"><input name="edf" type="EDF" nodename="edf" /></surface>
  <surfacematerial name="M" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="surf" />
  </surfacematerial>
</materialx>
"#,
    )
    .expect("write mtlx");
    let stage = d.join("stage.usda");
    std::fs::write(
        &stage,
        format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Scope "Looks"
    {{
        def Material "M" (
            prepend references = @{}@</MaterialX/Materials/M>
        )
        {{
        }}
    }}
{body}
}}
"#,
            mtlx.display()
        ),
    )
    .expect("write stage");
    Scene::from_usd(&stage).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A mesh prim `name` of the two quads, translated by `dy` along y, as a
/// `none` mesh unless `extra` authors a scheme, with `extra` authored on it.
fn quads(name: &str, dy: f32, extra: &str) -> String {
    let scheme = if extra.contains("subdivisionScheme") {
        ""
    } else {
        r#"uniform token subdivisionScheme = "none""#
    };
    format!(
        r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        double3 xformOp:translate = (0, {dy}, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (0, 1, 0), (1, 1, 0), (2, 1, 0)]
        {scheme}
        rel material:binding = </World/Looks/M>
        {extra}
    }}"#
    )
}

/// The first hit straight down at `(x, y)`: its shading normal and what the
/// material emits there.
fn at(scene: &Scene, x: f32, y: f32) -> (Vec3A, Vec3A) {
    let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z);
    let hit = scene
        .world
        .intersect(&ray, 1e-4, 1e4)
        .unwrap_or_else(|| panic!("no hit at ({x}, {y})"));
    (hit.rec.normal, hit.mat.emitted_at(&ray, &hit.rec, 1.0))
}

fn normal_at(scene: &Scene, x: f32, y: f32) -> Vec3A {
    at(scene, x, y).0
}

fn close(a: Vec3A, b: Vec3A) -> bool {
    (a - b).abs().max_element() <= 1e-4
}

#[track_caller]
fn assert_normal(scene: &Scene, x: f32, y: f32, want: Vec3A) {
    let got = normal_at(scene, x, y);
    assert!(close(got, want), "({x}, {y}): {got}, want {want}");
}

fn warned(scene: &Scene, code: WarningCode) -> Vec<String> {
    scene
        .warnings
        .iter()
        .filter(|w| w.code == code)
        .map(|w| w.message.clone())
        .collect()
}

/// Tilted toward -x and +x, and straight up.
const LEFT: Vec3A = Vec3A::new(-0.6, 0.0, 0.8);
const RIGHT: Vec3A = Vec3A::new(0.6, 0.0, 0.8);
const UP: Vec3A = Vec3A::Z;

/// Hard-edged: every corner of the first quad `LEFT`, of the second `RIGHT`.
const HARD: &str = r#"normal3f[] normals = [(-0.6, 0, 0.8), (-0.6, 0, 0.8), (-0.6, 0, 0.8), (-0.6, 0, 0.8), (0.6, 0, 0.8), (0.6, 0, 0.8), (0.6, 0, 0.8), (0.6, 0, 0.8)] (interpolation = "faceVarying")"#;

/// `x + 2y` at every point: a linear function, which every triangulation
/// interpolates exactly.
const RAMP: &str = "float[] primvars:ramp = [0, 1, 2, 2, 3, 4] (interpolation = \"vertex\")";

#[test]
fn face_varying_normals_keep_a_hard_edge() {
    let s = load("face_varying", &quads("Q", 0.0, HARD));
    // Right up to the shared edge from either side: the points at x = 1 are
    // split, so neither face blends toward the other's normal.
    for y in [0.2, 0.5, 0.8] {
        assert_normal(&s, 0.1, y, LEFT);
        assert_normal(&s, 0.99, y, LEFT);
        assert_normal(&s, 1.01, y, RIGHT);
        assert_normal(&s, 1.9, y, RIGHT);
    }
    assert!(warned(&s, WarningCode::MeshInvalidNormals).is_empty());
}

#[test]
fn splitting_points_keeps_vertex_primvars_on_their_points() {
    let s = load("split_ramp", &quads("Q", 0.0, &format!("{HARD}\n{RAMP}")));
    for (x, y) in [
        (0.25, 0.25),
        (0.99, 0.5),
        (1.01, 0.5),
        (1.9, 0.9),
        (0.1, 0.7),
    ] {
        let (_, emitted) = at(&s, x, y);
        let want = Vec3A::splat(x + 2.0 * y);
        assert!(
            (emitted - want).abs().max_element() <= 1e-4,
            "({x}, {y}): {emitted}, want {want}"
        );
    }
}

#[test]
fn indexed_face_varying_normals() {
    let s = load(
        "indexed",
        &quads(
            "Q",
            0.0,
            r#"normal3f[] primvars:normals = [(-0.6, 0, 0.8), (0.6, 0, 0.8)] (interpolation = "faceVarying")
        int[] primvars:normals:indices = [0, 0, 0, 0, 1, 1, 1, 1]"#,
        ),
    );
    assert_normal(&s, 0.5, 0.5, LEFT);
    assert_normal(&s, 1.5, 0.5, RIGHT);
}

#[test]
fn vertex_normals_interpolate_smoothly() {
    // Normals depending on x alone, so both triangles of each quad
    // interpolate them alike: at x = 0.5 halfway between LEFT and UP.
    let s = load(
        "vertex",
        &quads(
            "Q",
            0.0,
            r#"normal3f[] normals = [(-0.6, 0, 0.8), (0, 0, 1), (0.6, 0, 0.8), (-0.6, 0, 0.8), (0, 0, 1), (0.6, 0, 0.8)]"#,
        ),
    );
    assert_normal(&s, 1.0, 0.5, UP);
    assert_normal(&s, 0.5, 0.3, ((LEFT + UP) * 0.5).normalize());
    assert_normal(&s, 1.5, 0.7, ((RIGHT + UP) * 0.5).normalize());
}

#[test]
fn uniform_and_constant_normals() {
    let s = load(
        "uniform_constant",
        &format!(
            "{}{}",
            quads(
                "U",
                0.0,
                r#"normal3f[] normals = [(-0.6, 0, 0.8), (0.6, 0, 0.8)] (interpolation = "uniform")"#,
            ),
            quads(
                "C",
                3.0,
                r#"normal3f[] primvars:normals = [(0.6, 0, 0.8)] (interpolation = "constant")"#,
            ),
        ),
    );
    assert_normal(&s, 0.9, 0.5, LEFT);
    assert_normal(&s, 1.1, 0.5, RIGHT);
    assert_normal(&s, 0.5, 3.5, RIGHT);
    assert_normal(&s, 1.5, 3.5, RIGHT);
}

#[test]
fn primvars_normals_win_over_normals() {
    let s = load(
        "precedence",
        &quads(
            "Q",
            0.0,
            r#"normal3f[] normals = [(0, 0, 1), (0, 0, 1)] (interpolation = "uniform")
        normal3f[] primvars:normals = [(-0.6, 0, 0.8), (0.6, 0, 0.8)] (interpolation = "uniform")"#,
        ),
    );
    assert_normal(&s, 0.5, 0.5, LEFT);
    assert_normal(&s, 1.5, 0.5, RIGHT);
}

#[test]
fn identical_meshes_with_different_normals_do_not_share() {
    // Same points, topology and material, so they would share one slot if
    // the normals were not part of its identity.
    let s = load(
        "dedupe",
        &format!(
            "{}{}{}",
            quads("A", 0.0, HARD),
            quads(
                "B",
                3.0,
                r#"normal3f[] normals = [(0, 0, 1)] (interpolation = "constant")"#,
            ),
            quads("C", 6.0, ""),
        ),
    );
    assert_normal(&s, 0.5, 0.5, LEFT);
    assert_normal(&s, 0.5, 3.5, UP);
    assert_normal(&s, 0.5, 6.5, UP);
}

#[test]
fn normals_scale_by_the_inverse_transpose() {
    // Stretched 2x along x: the inverse transpose halves a normal's x, so
    // (-0.6, 0, 0.8) shades as (-0.3, 0, 0.8), normalized.
    let s = load(
        "inverse_transpose",
        &quads("Q", 0.0, HARD).replace(
            r#"double3 xformOp:translate = (0, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]"#,
            r#"float3 xformOp:scale = (2, 1, 1)
        uniform token[] xformOpOrder = ["xformOp:scale"]"#,
        ),
    );
    assert_normal(&s, 1.0, 0.5, Vec3A::new(-0.3, 0.0, 0.8).normalize());
    assert_normal(&s, 3.0, 0.5, Vec3A::new(0.3, 0.0, 0.8).normalize());
}

#[test]
fn a_subdivision_mesh_ignores_authored_normals() {
    let s = load(
        "subdiv",
        &quads(
            "Q",
            0.0,
            &format!("uniform token subdivisionScheme = \"catmullClark\"\n        {HARD}"),
        ),
    );
    // Its smooth cage normals: the plane's own.
    assert_normal(&s, 0.5, 0.5, UP);
    assert_normal(&s, 1.5, 0.5, UP);
}

#[test]
fn unreadable_normals_warn_and_render_faceted() {
    let s = load(
        "refused",
        &format!(
            "{}{}",
            quads(
                "Short",
                0.0,
                r#"normal3f[] normals = [(-0.6, 0, 0.8), (0.6, 0, 0.8)] (interpolation = "faceVarying")"#,
            ),
            quads(
                "Flat",
                3.0,
                r#"float[] primvars:normals = [1, 2] (interpolation = "uniform")"#,
            ),
        ),
    );
    assert_normal(&s, 0.5, 0.5, UP);
    assert_normal(&s, 0.5, 3.5, UP);
    // One warning per cause, naming both prims.
    let w: Vec<_> = s
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::MeshInvalidNormals)
        .collect();
    assert_eq!(w.len(), 1, "{:?}", s.warnings);
    assert_eq!(w[0].count, 2, "{w:?}");
    for prim in ["/World/Short", "/World/Flat"] {
        assert!(w[0].prims.iter().any(|p| p == prim), "{w:?}");
    }
}

#[test]
fn a_zero_normal_reads_its_points_smooth_normal() {
    let s = load(
        "zero",
        &quads(
            "Q",
            0.0,
            r#"normal3f[] normals = [(0, 0, 0), (-0.6, 0, 0.8)] (interpolation = "uniform")"#,
        ),
    );
    assert_normal(&s, 0.5, 0.5, UP);
    assert_normal(&s, 1.5, 0.5, LEFT);
}

#[test]
fn face_varying_normals_follow_a_left_handed_meshs_reversed_faces() {
    // Per-corner normals tilted by each corner's x, in the authored
    // face-vertex order: read against the reversed faces' slots they would
    // land on the wrong corners. The normals name the front themselves, so
    // they keep their directions.
    let fv = r#"normal3f[] normals = [(-0.6, 0, 0.8), (0, 0, 1), (0, 0, 1), (-0.6, 0, 0.8), (0, 0, 1), (0.6, 0, 0.8), (0.6, 0, 0.8), (0, 0, 1)] (interpolation = "faceVarying")"#;
    for orientation in ["rightHanded", "leftHanded"] {
        let s = load(
            &format!("fv_{orientation}"),
            &quads(
                "Q",
                0.0,
                &format!("uniform token orientation = \"{orientation}\"\n        {fv}"),
            ),
        );
        assert_normal(&s, 0.5, 0.3, ((LEFT + UP) * 0.5).normalize());
        assert_normal(&s, 1.5, 0.7, ((RIGHT + UP) * 0.5).normalize());
        assert_normal(&s, 1.0, 0.5, UP);
    }
}
