//! Point instancers and native instances nested in each other, as ALab's
//! vegetation nests them: a `PointInstancer` inside an instance, and one whose
//! prototype is itself an instance, whose prims are then reached as instance
//! proxies. Import parity compares each scene with the direct importer's
//! baseline (`parity`).

use crust_core::{Scene, WarningCode};

const STAGE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "Plant" (
    instanceable = true
)
{
    def PointInstancer "Leaves"
    {
        rel prototypes = [</Plant/Leaves/Prototypes/Leaf>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes"
        {
            def Mesh "Leaf"
            {
                int[] faceVertexCounts = [4]
                int[] faceVertexIndices = [0, 1, 2, 3]
                point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
                uniform token subdivisionScheme = "none"
            }
        }
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "A" (
        prepend references = </Plant>
        instanceable = true
    )
    {
    }
    def Xform "B" (
        prepend references = </Plant>
        instanceable = true
    )
    {
        double3 xformOp:translate = (0, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;

#[test]
fn a_point_instancer_inside_an_instance_places_its_prototypes() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("plants.usda");
    std::fs::write(&path, STAGE).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let empty = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::InstancingEmptyPrototype);
    assert!(empty.is_none(), "{empty:?}");
    assert!(scene.world.count() > 0, "the leaves are placed");
}

/// A point instancer whose prototype is an instanceable reference: the
/// prototype's mesh is an instance proxy under it.
const INSTANCED_PROTOTYPE: &str = r#"#usda 1.0
def Xform "LeafAsset"
{
    def Mesh "Geo"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
    }
}
def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def PointInstancer "Leaves"
    {
        rel prototypes = [</World/Leaves/Prototypes/Leaf>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Leaf" (
                prepend references = </LeafAsset>
                instanceable = true
            )
            {
            }
        }
    }
}
"#;

#[test]
fn an_instanced_point_instancer_prototype_is_read_through_its_proxies() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("instanced_prototype.usda");
    std::fs::write(&path, INSTANCED_PROTOTYPE).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let empty = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::InstancingEmptyPrototype);
    assert!(empty.is_none(), "{empty:?}");
    assert!(scene.world.count() > 1, "the leaves are placed");
}

/// [`INSTANCED_PROTOTYPE`] with a material bound inside the instanced asset:
/// its binding is resolved in the proxy's namespace, as the stage names it.
const BOUND_INSTANCED_PROTOTYPE: &str = r#"#usda 1.0
def Xform "LeafAsset"
{
    def Mesh "Geo" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
        rel material:binding = </LeafAsset/Looks/Green>
    }
    def Scope "Looks"
    {
        def Material "Green"
        {
            token outputs:surface.connect = </LeafAsset/Looks/Green/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.1, 0.6, 0.1)
                token outputs:surface
            }
        }
    }
}
def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def PointInstancer "Leaves"
    {
        rel prototypes = [</World/Leaves/Prototypes/Leaf>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Leaf" (
                prepend references = </LeafAsset>
                instanceable = true
            )
            {
            }
        }
    }
}
"#;

#[test]
fn a_binding_inside_an_instanced_prototype_is_named_in_the_proxys_namespace() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("bound_instanced_prototype.usda");
    std::fs::write(&path, BOUND_INSTANCED_PROTOTYPE).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let fallback = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::MaterialFallbackDefault);
    assert!(fallback.is_none(), "{fallback:?}");
    assert!(scene.world.count() > 1, "the leaves are placed");
}
