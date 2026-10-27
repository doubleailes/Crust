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

/// Two point instancers whose prototypes are instanceable references to one
/// asset, each prototype binding a material it carries — ALab's bonsai
/// generations. The asset is one USD prototype, read once through Moho's
/// propagated prototype, its material resolved once and its mesh shared by
/// both instancers (the change's third behaviour change, D3c): the direct
/// importer walked each instancer's prototype through its instance proxies
/// and resolved the material once per instancer. The image is the same.
const SHARED_ASSET: &str = r#"#usda 1.0
def Xform "Branch"
{
    def Mesh "Geo" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
        rel material:binding = </Branch/Looks/Bark>
    }
    def Scope "Looks"
    {
        def Material "Bark"
        {
            token outputs:surface.connect = </Branch/Looks/Bark/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.4, 0.25, 0.1)
                token outputs:surface
            }
        }
    }
}
def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1, 1, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def PointInstancer "Generation1"
    {
        rel prototypes = [</World/Generation1/Prototypes/Branch>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Branch" (prepend references = </Branch> instanceable = true) {}
        }
    }
    def PointInstancer "Generation2"
    {
        rel prototypes = [</World/Generation2/Prototypes/Branch>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 2, 0), (1, 2, 0)]
        def Scope "Prototypes"
        {
            def Xform "Branch" (prepend references = </Branch> instanceable = true) {}
        }
    }
}
"#;

#[test]
fn instancers_of_one_instanced_asset_share_its_prototype() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("shared_asset.usda");
    std::fs::write(&path, SHARED_ASSET).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    // Both instancers read the one native prototype of `/Branch`, so its
    // material is converted once and shared (D3c); the digest holds that.
    assert_eq!(
        scene.world.count(),
        5,
        "two copies per instancer, and the asset itself"
    );
}
