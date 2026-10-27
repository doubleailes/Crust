//! Materials a streamed chunk binds from outside itself (the `moho-import`
//! change, task 9.4): from the payload-free scene input when no payload
//! hides them, from the side input over a stage masked to them when one
//! does. Each import is checked against the direct importer's baseline
//! (`crust_core::parity`), and each quad's colour says which material it
//! got.

#![allow(clippy::unwrap_used)]

use crust_core::{MASK_CAMERA, Ray, Scene, Vec3A, WarningCode};

fn quad(name: &str, x: f32, mat: &str) -> String {
    format!(
        r#"
    def Xform "{name}"
    {{
        def Mesh "Quad" (prepend apiSchemas = ["MaterialBindingAPI"])
        {{
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 1, 0), ({x0}, 1, 0)]
            rel material:binding = <{mat}>
        }}
    }}"#,
        x0 = x,
        x1 = x + 1.0
    )
}

fn material(name: &str, rgb: (f32, f32, f32)) -> String {
    format!(
        r#"
        def Material "{name}"
        {{
            token outputs:surface.connect = <S.outputs:surface>
            def Shader "S"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = ({}, {}, {})
                float inputs:roughness = 1
                token outputs:surface
            }}
        }}"#,
        rgb.0, rgb.1, rgb.2
    )
}

/// The reflectance of the quad under `x`.
fn reflectance(scene: &Scene, x: f32) -> Vec3A {
    let r = Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("hits the quad");
    hit.mat.eval(&r, &hit.rec, Vec3A::Z).expect("a lobe").0
}

fn no_fallback(scene: &Scene) {
    let fallbacks: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::MaterialFallbackDefault)
        .collect();
    assert!(fallbacks.is_empty(), "{fallbacks:#?}");
}

/// `/World/Looks` beside four geometry chunks: five children of one root,
/// so each is a masked chunk, and the quads' material is on none of their
/// stages. No payload hides it, so the payload-free scene input holds it.
#[test]
fn materials_beside_four_geometry_chunks_resolve_from_the_scene_input() {
    let dir = std::env::temp_dir().join("crust_streamed_materials_beside");
    std::fs::create_dir_all(&dir).unwrap();
    let stage = format!(
        "#usda 1.0\ndef Xform \"World\"\n{{\n    def Scope \"Looks\"\n    {{{}\n    }}{}{}{}{}\n}}\n",
        material("Red", (0.8, 0.1, 0.1)),
        quad("A", -3.0, "/World/Looks/Red"),
        quad("B", -1.0, "/World/Looks/Red"),
        quad("C", 1.0, "/World/Looks/Red"),
        quad("D", 3.0, "/World/Looks/Red"),
    );
    let path = dir.join("beside.usda");
    std::fs::write(&path, stage).unwrap();
    let scene = Scene::from_usd(&path).unwrap();
    for x in [-2.5, -0.5, 1.5, 3.5] {
        let f = reflectance(&scene, x);
        assert!(f.x > 4.0 * f.y && f.x > 4.0 * f.z, "red at x = {x}: {f}");
    }
    no_fallback(&scene);
}

/// A material that lives only inside another chunk's payload: `/World/B`
/// loads `b.usda`, which holds `Looks/Blue` and B's own quad, and A binds
/// that material. The scene input does not load B's payload, so A's chunk
/// reads the material from the side input, over a stage masked to it.
#[test]
fn a_material_inside_another_chunks_payload_resolves_from_the_side_input() {
    let dir = std::env::temp_dir().join("crust_streamed_materials_payload");
    std::fs::create_dir_all(&dir).unwrap();
    let b = format!(
        "#usda 1.0\n(\n    defaultPrim = \"B\"\n)\ndef Xform \"B\"\n{{\n    def Scope \"Looks\"\n    {{{}\n    }}{}\n}}\n",
        material("Blue", (0.1, 0.1, 0.8)),
        quad("Geo", -1.0, "/B/Looks/Blue"),
    );
    std::fs::write(dir.join("b.usda"), b).unwrap();
    let stage = format!(
        "#usda 1.0\ndef Xform \"World\"\n{{{}\n    def Xform \"B\" (prepend payload = @./b.usda@)\n    {{\n    }}{}{}\n}}\n",
        quad("A", -3.0, "/World/B/Looks/Blue"),
        quad("C", 1.0, "/World/B/Looks/Blue"),
        quad("D", 3.0, "/World/B/Looks/Blue"),
    );
    let path = dir.join("payload.usda");
    std::fs::write(&path, stage).unwrap();
    let scene = Scene::from_usd(&path).unwrap();
    for x in [-2.5, -0.5, 1.5, 3.5] {
        let f = reflectance(&scene, x);
        assert!(f.z > 4.0 * f.x && f.z > 4.0 * f.y, "blue at x = {x}: {f}");
    }
    no_fallback(&scene);
}
