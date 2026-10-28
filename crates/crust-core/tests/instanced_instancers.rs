//! Point instancers and native instances nested in each other, as ALab's
//! vegetation nests them: a `PointInstancer` inside an instance, and one whose
//! prototype is itself an instance, whose prims are then reached as instance
//! proxies.

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

/// A point instancer whose prototypes are prims *inside* a native instance —
/// Moana's coral: `Prototypes/Archive` is instanceable, so each target is an
/// instance proxy. The direct importer read the proxied meshes on the stage.
const PROXY_TARGETS: &str = r#"#usda 1.0
def Xform "ArchiveAsset"
{
    def Mesh "Piece1"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
    }
    def Mesh "Piece2"
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
    }
}
def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (1, 1, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def PointInstancer "Scatter"
    {
        rel prototypes = [</World/Scatter/Prototypes/Archive/Piece1>, </World/Scatter/Prototypes/Archive/Piece2>]
        int[] protoIndices = [0, 1, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Archive" (prepend references = </ArchiveAsset> instanceable = true) {}
        }
    }
}
"#;

/// Moho's M12: point instancing copies an instance-proxy target from the
/// instance's propagated prototype (`<instancer>/ForInstancer<hash>/prototype<i>`),
/// so the import places the proxied meshes, equal to the direct importer's.
#[test]
fn a_point_instancer_targeting_prims_inside_an_instance_places_them() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("proxy_targets.usda");
    std::fs::write(&path, PROXY_TARGETS).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let empty = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::InstancingEmptyPrototype);
    assert!(empty.is_none(), "{empty:?}");
}

/// [`PROXY_TARGETS`] with a material inside the instanced asset, bound by the
/// targets: Moana's coral binds each archive piece to a material of its own
/// archive. The copy's binding names the material in the instance's
/// propagated prototype, outside the copy; it is read there and named under
/// the instance, as the direct importer read it through the proxy.
const PROXY_TARGET_MATERIALS: &str = r#"#usda 1.0
def Xform "ArchiveAsset"
{
    def Mesh "Piece1" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
        rel material:binding = </ArchiveAsset/Looks/Red>
    }
    def Mesh "Piece2" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
        rel material:binding = </ArchiveAsset/Looks/Blue>
    }
    def Scope "Looks"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </ArchiveAsset/Looks/Red/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }
        }
        def Material "Blue"
        {
            token outputs:surface.connect = </ArchiveAsset/Looks/Blue/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.1, 0.1, 0.8)
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
    def PointInstancer "Scatter"
    {
        rel prototypes = [</World/Scatter/Prototypes/Archive/Piece1>, </World/Scatter/Prototypes/Archive/Piece2>]
        int[] protoIndices = [0, 1, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Archive" (prepend references = </ArchiveAsset> instanceable = true) {}
        }
    }
}
"#;

#[test]
fn a_point_instancer_target_inside_an_instance_keeps_its_material() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("proxy_target_materials.usda");
    std::fs::write(&path, PROXY_TARGET_MATERIALS).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let fallback = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::MaterialFallbackDefault);
    assert!(fallback.is_none(), "{fallback:?}");
}

/// [`PROXY_TARGET_MATERIALS`]' instancer inside an instanced element, as
/// Moana's coral elements are: the instancer's targets are held re-rooted in
/// the element's propagated prototype, and the archive instance they lie in
/// is named under the element's USD prototype.
const NESTED_PROXY_TARGET_MATERIALS: &str = r#"#usda 1.0
def Xform "ArchiveAsset"
{
    def Mesh "Piece1" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]
        uniform token subdivisionScheme = "none"
        rel material:binding = </ArchiveAsset/Looks/Red>
    }
    def Scope "Looks"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </ArchiveAsset/Looks/Red/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }
        }
    }
}
def Xform "Element"
{
    def PointInstancer "Scatter"
    {
        rel prototypes = [</Element/Scatter/Prototypes/Archive/Piece1>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0)]
        def Scope "Prototypes"
        {
            def Xform "Archive" (prepend references = </ArchiveAsset> instanceable = true) {}
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
    def Xform "E1" (prepend references = </Element> instanceable = true) {}
    def Xform "E2" (prepend references = </Element> instanceable = true)
    {
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;

#[test]
fn an_instanced_point_instancer_target_inside_an_instance_keeps_its_material() {
    let dir = std::env::temp_dir().join("crust_instanced_instancers");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("nested_proxy_target_materials.usda");
    std::fs::write(&path, NESTED_PROXY_TARGET_MATERIALS).expect("write stage");
    let scene = Scene::from_usd(&path).expect("imports");
    let fallback = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::MaterialFallbackDefault);
    assert!(fallback.is_none(), "{fallback:?}");
}
