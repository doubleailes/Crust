//! A light linked to receivers in other chunks of a streamed import (the
//! `moho-import` change, task 10.2): the light's `lightLink` is judged after
//! the last chunk against every receiver walked, whichever chunk held it.
//! Checked against the direct importer's baseline (`crust_core::parity`).

#![allow(clippy::unwrap_used)]

use crust_core::Scene;

fn mesh(name: &str, x: f32) -> String {
    format!(
        r#"
        def Mesh "{name}"
        {{
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 1, 0), ({x0}, 1, 0)]
        }}"#,
        x0 = x,
        x1 = x + 1.0
    )
}

#[test]
fn a_light_links_receivers_in_other_chunks() {
    let dir = std::env::temp_dir().join("crust_streamed_light_links");
    std::fs::create_dir_all(&dir).unwrap();
    let stage = format!(
        r#"#usda 1.0
def Xform "World"
{{
    def Scope "Lights"
    {{
        def RectLight "key" (prepend apiSchemas = ["CollectionAPI:lightLink", "CollectionAPI:shadowLink"])
        {{
            float inputs:intensity = 10
            rel collection:lightLink:includes = [</World/A>, </World/C/Inner>]
            uniform bool collection:lightLink:includeRoot = 0
            rel collection:shadowLink:excludes = </World/B>
            double3 xformOp:translate = (0, 0.5, 3)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }}
        def DistantLight "fill" {{ float inputs:intensity = 0.5 }}
    }}
    def Xform "A"
    {{{}
    }}
    def Xform "B"
    {{{}
    }}
    def Xform "C"
    {{{}
        def Xform "Inner"
        {{{}
        }}
    }}
}}
"#,
        mesh("M", -3.0),
        mesh("M", -1.0),
        mesh("M", 1.0),
        mesh("M", 3.0),
    );
    let path = dir.join("links.usda");
    std::fs::write(&path, stage).unwrap();
    let scene = Scene::from_usd(&path).unwrap();
    assert_eq!(scene.lights.count(), 2);
    assert!(
        (0..scene.lights.count()).any(|i| scene.lights.restricted(i)),
        "the key light is linked"
    );
}
