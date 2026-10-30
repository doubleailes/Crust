//! A transform stack openusd cannot compose — the inverse of a singular
//! `transform` — imports as the index composes it, without a warning: the
//! index gives a composed matrix only, so the reason openusd refused the
//! stack is no fact the delegate reads (`host-facts-mtlx-only`).

#![allow(clippy::unwrap_used)]

use crust_core::Scene;

#[test]
fn an_uncomposable_stack_imports_without_a_warning() {
    let dir = std::env::temp_dir().join("crust_xform_uncomposable");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("singular.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
def Xform "World"
{
    def Xform "Bad"
    {
        matrix4d xformOp:transform = ((0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 1))
        uniform token[] xformOpOrder = ["!invert!xformOp:transform"]
        def Mesh "M"
        {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        }
    }
}
"#,
    )
    .unwrap();
    let scene = Scene::from_usd(&path).unwrap();
    assert!(
        scene
            .warnings
            .iter()
            .all(|w| !w.prims.iter().any(|p| p.starts_with("/World/Bad"))),
        "{:#?}",
        scene.warnings
    );
}
