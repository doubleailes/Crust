//! Relative asset paths authored in a referenced layer, below the root
//! layer's directory, are anchored against the layer that authors them, as
//! USD anchors them: a UsdUVTexture's file (a UDIM set and a plain file, both
//! absent from disk, so openusd resolves neither), a MaterialX `filename`
//! input, and a RectLight's texture. The test asserts the paths the host is
//! asked for.

#![allow(clippy::unwrap_used)]

use crust_core::{AssetLoader, ColorSpace, Scene};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const ASSET: &str = r#"#usda 1.0
def Xform "Asset"
{
    def Mesh "Q" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "vertex")
        rel material:binding = </Asset/Looks/M>
    }
    def Mesh "R" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(2, 0, 0), (3, 0, 0), (3, 1, 0), (2, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "vertex")
        rel material:binding = </Asset/Looks/X>
    }
    def RectLight "Card"
    {
        float inputs:width = 1
        float inputs:height = 1
        asset inputs:texture:file = @../cards/card.png@
        double3 xformOp:translate = (1, 1, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </Asset/Looks/M/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Asset/Looks/M/T.outputs:rgb>
                float inputs:roughness.connect = </Asset/Looks/M/U.outputs:r>
                token outputs:surface
            }
            def Shader "T"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @../tex/albedo.<UDIM>.png@
                vector3f outputs:rgb
            }
            def Shader "U"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @textures/rough.png@
                float outputs:r
            }
        }
        def Material "X"
        {
            token outputs:mtlx:surface.connect = </Asset/Looks/X/S.outputs:out>
            def Shader "S"
            {
                uniform token info:id = "ND_standard_surface_surfaceshader"
                color3f inputs:base_color.connect = </Asset/Looks/X/I.outputs:out>
                token outputs:out
            }
            def Shader "I"
            {
                uniform token info:id = "ND_image_color3"
                asset inputs:file = @textures/base.<UDIM>.png@
                color3f outputs:out
            }
        }
    }
}
"#;

const ROOT: &str = r#"#usda 1.0
def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1.5, 0.5, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "Thing" (prepend references = @assets/deep/asset.usda@</Asset>) {}
}
"#;

/// The stage, written under a fresh directory: `root.usda`, and the asset two
/// directories below it.
fn stage() -> PathBuf {
    let dir = std::env::temp_dir().join("crust_asset_anchoring");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets/deep")).unwrap();
    std::fs::write(dir.join("assets/deep/asset.usda"), ASSET).unwrap();
    std::fs::write(dir.join("root.usda"), ROOT).unwrap();
    dir.join("root.usda")
}

/// A host that records every texture path it is asked for, and decodes none.
#[derive(Default)]
struct Recording(Mutex<Vec<PathBuf>>);

impl AssetLoader for Recording {
    fn load_environment(&self, path: &Path, _: ColorSpace) -> Option<crust_core::EnvironmentMap> {
        self.0.lock().unwrap().push(path.to_path_buf());
        None
    }
    fn load_texture(&self, path: &Path, _: ColorSpace) -> Option<Arc<dyn crust_core::Texture2D>> {
        self.0.lock().unwrap().push(path.to_path_buf());
        None
    }
    fn load_light_texture(
        &self,
        path: &Path,
        _: ColorSpace,
    ) -> Option<Arc<crust_core::LightTexture>> {
        self.0.lock().unwrap().push(path.to_path_buf());
        None
    }
}

#[test]
fn relative_assets_in_a_referenced_layer_anchor_on_that_layer() {
    let path = stage();
    let dir = path.parent().unwrap().display().to_string();
    let assets = Recording::default();
    Scene::from_usd_with_assets(&path, &assets).expect("imports");
    let mut asked: Vec<String> = assets
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.display().to_string().replacen(&dir, "$DIR", 1))
        .collect();
    asked.sort();
    asked.dedup();
    // Each anchored on `assets/deep/`, the authoring layer's directory, as
    // USD anchors it, never on the root layer's.
    assert_eq!(
        asked,
        [
            "$DIR/assets/deep/../cards/card.png",
            "$DIR/assets/deep/../tex/albedo.<UDIM>.png",
            "$DIR/assets/deep/textures/base.<UDIM>.png",
            "$DIR/assets/deep/textures/rough.png",
        ]
    );
}

/// The textures the host is asked for whose name holds `needle`, with `root`
/// written as `$DIR` and every separator as `/`.
fn asked(assets: &Recording, root: &Path, needle: &str) -> Vec<String> {
    let root = root.display().to_string();
    assets
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|p| {
            p.display()
                .to_string()
                .replacen(&root, "$DIR", 1)
                .replace('\\', "/")
        })
        .filter(|p| p.contains(needle))
        .collect()
}

/// How many `asset.string_path` warnings `scene` raised.
fn string_path_warnings(scene: &Scene) -> usize {
    scene
        .warnings
        .iter()
        .filter(|w| w.code == crust_core::WarningCode::AssetStringPath)
        .count()
}

/// A texture file authored as a `string`, not an `asset`: USD does not
/// anchor text on its layer, and the index carries no resolution for it, so
/// it resolves against the stage's directory and says so
/// (`hydra-only-renders`, design D1).
#[test]
fn a_string_typed_texture_path_resolves_against_the_stage_and_warns() {
    let dir = std::env::temp_dir().join("crust_asset_anchoring_string");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets/deep")).unwrap();
    let asset = ASSET.replace(
        "asset inputs:file = @textures/rough.png@",
        "string inputs:file = \"textures/rough.png\"",
    );
    std::fs::write(dir.join("assets/deep/asset.usda"), asset).unwrap();
    std::fs::write(dir.join("root.usda"), ROOT).unwrap();
    let assets = Recording::default();
    let scene = Scene::from_usd_with_assets(&dir.join("root.usda"), &assets).expect("imports");
    assert_eq!(asked(&assets, &dir, "rough"), ["$DIR/textures/rough.png"]);
    assert!(string_path_warnings(&scene) >= 1, "{:?}", scene.warnings);
}

/// An absolute path authored as a `string` is read as it is, silently.
#[test]
fn an_absolute_string_texture_path_is_read_as_it_is() {
    let dir = std::env::temp_dir().join("crust_asset_anchoring_absolute");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets/deep")).unwrap();
    let absolute = dir.join("elsewhere/rough.png");
    let literal = absolute.display().to_string().replace('\\', "/");
    let asset = ASSET.replace(
        "asset inputs:file = @textures/rough.png@",
        &format!("string inputs:file = \"{literal}\""),
    );
    std::fs::write(dir.join("assets/deep/asset.usda"), asset).unwrap();
    std::fs::write(dir.join("root.usda"), ROOT).unwrap();
    let assets = Recording::default();
    let scene = Scene::from_usd_with_assets(&dir.join("root.usda"), &assets).expect("imports");
    let asked: Vec<String> = assets
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.display().to_string().replace('\\', "/"))
        .filter(|p| p.contains("rough"))
        .collect();
    assert_eq!(asked, [literal]);
    assert_eq!(string_path_warnings(&scene), 0, "{:?}", scene.warnings);
}
