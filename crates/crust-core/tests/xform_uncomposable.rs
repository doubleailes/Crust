//! A transform stack openusd cannot compose — the inverse of a singular
//! `transform` — keeps its parent's transform with an `xform.uncomposable`
//! warning quoting openusd's reason (the `moho-import` change reads that
//! reason off the stage, design D7), warning text included.

#![allow(clippy::unwrap_used)]

use crust_core::{Scene, WarningCode};

#[test]
fn an_uncomposable_stack_warns_with_openusds_reason() {
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
    let warned: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::XformUncomposable)
        .collect();
    assert_eq!(warned.len(), 1, "{:#?}", scene.warnings);
    assert!(
        warned[0]
            .message
            .contains("could not compose its xformOp stack"),
        "{}",
        warned[0].message
    );
}
