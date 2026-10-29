//! The delegate's updates (design D9): the provenance a commit records, the
//! material and light swaps a world and light list take or refuse, and each
//! class of update as a bitwise pair — the delegate updated in place against
//! a fresh delegate over the same index (render-delegate spec, "Updates apply
//! in place from the dirty locators").

use crust_core::live::Provenance;
use crust_core::{Emissive, LightKind, Material, OpenPBR, Scene, Vec3A};
use std::path::{Path, PathBuf};

mod common;
use common::{Host, bits, write};
use crust_core::UpdateClass;
use std::sync::Arc;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

fn ptr(m: &Arc<dyn Material>) -> *const () {
    Arc::as_ptr(m) as *const ()
}

/// What every provenance must say of its scene: each material's slots hold
/// one `Arc`, no slot belongs to two materials, each slot's prim is known,
/// and each area light's geometry carries the light's own `Emissive`.
fn consistent(scene: &Scene) -> &Provenance {
    let prov = scene
        .provenance
        .as_ref()
        .expect("an imported scene has one");
    let materials = scene.world.materials();
    let mut owner = vec![None; materials.len()];
    for (path, origin) in prov.materials() {
        assert!(!origin.slots.is_empty(), "{path}");
        let first = ptr(&materials[origin.slots[0] as usize]);
        for &slot in &origin.slots {
            assert_eq!(ptr(&materials[slot as usize]), first, "{path} slot {slot}");
            assert!(
                owner[slot as usize].replace(path).is_none(),
                "slot {slot} twice"
            );
            assert!(prov.prim_of_geometry(slot).is_some(), "slot {slot}'s prim");
        }
    }
    for (path, origin) in prov.lights() {
        let light = &scene.lights.lights()[origin.index];
        if let LightKind::Area(area) = light {
            assert_eq!(origin.geoms.clone().count(), 1, "{path}");
            let slot = &materials[origin.geoms.start as usize];
            let emitter: Arc<dyn Material> = area.emitter().clone();
            assert_eq!(ptr(slot), ptr(&emitter), "{path}: one Emissive");
        } else {
            assert!(origin.geoms.is_empty(), "{path}");
        }
    }
    prov
}

#[test]
fn the_cornell_box_maps_every_material_and_light() {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    let prov = consistent(&scene);
    let looks = prov
        .material("/scene/Looks/initialShadingGroup")
        .expect("the shared material");
    assert_eq!(looks.slots.len(), 5, "its five meshes");
    assert_eq!(scene.world.count(), 5);
    assert_eq!(prov.light("/scene/Sky").map(|l| l.index), Some(0));
    assert_eq!(prov.camera(), Some("/scene/camera1"));
}

#[test]
fn an_instanced_stage_maps_its_slots() {
    let scene = Scene::from_usd(&sample("instancing.usda")).expect("imports");
    let prov = consistent(&scene);
    assert!(prov.materials().count() >= 2);
    let key = prov.light("/World/Key").expect("the rect light");
    assert_eq!(key.geoms.clone().count(), 1);
    assert_eq!(prov.render_settings(), Some("/Render/settings"));
}

fn swap_check(scene: &Scene, slots: &[u32], new: Arc<dyn Material>) -> bool {
    scene.world.check_materials(&[(slots, &new)]).is_ok()
}

/// A colour change keeps every flag the world was built on; a cutout or a
/// start of emission does not.
#[test]
fn a_material_swap_is_refused_when_it_moves_a_flag() {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    let slots = scene
        .provenance
        .as_ref()
        .unwrap()
        .material("/scene/Looks/initialShadingGroup")
        .unwrap()
        .slots
        .clone();
    let slots = slots.as_slice();
    let old = scene.world.material(slots[0]);
    assert_eq!(old.kind(), "OpenPBR", "the default grey");

    let recoloured = OpenPBR::diffuse(Vec3A::new(0.1, 0.6, 0.2));
    assert!(swap_check(&scene, slots, Arc::new(recoloured.clone())));

    let cutout = OpenPBR {
        geometry_opacity: 0.5,
        ..recoloured.clone()
    };
    assert!(!swap_check(&scene, slots, Arc::new(cutout)));

    let emitting = OpenPBR {
        emission_luminance: 1.0,
        emission_color: Vec3A::ONE,
        ..recoloured
    };
    assert!(!swap_check(&scene, slots, Arc::new(emitting)));

    let mut world_scene = scene;
    let new: Arc<dyn Material> = Arc::new(OpenPBR::diffuse(Vec3A::new(0.9, 0.1, 0.1)));
    world_scene.world.replace_materials(&[(slots, &new)]);
    for &slot in slots {
        assert_eq!(
            ptr(&world_scene.world.materials()[slot as usize]),
            ptr(&new)
        );
    }
}

/// A light replaced in the list keeps its place, and its `Emissive` is the
/// one its geometry's slot holds, so the light and the surface answer
/// `radiance_toward` alike.
#[test]
fn a_replaced_light_and_its_geometry_share_one_emissive() {
    let mut scene = Scene::from_usd(&sample("instancing.usda")).expect("imports");
    let key = scene
        .provenance
        .as_ref()
        .unwrap()
        .light("/World/Key")
        .unwrap()
        .clone();
    let LightKind::Area(old) = scene.lights.lights()[key.index].clone() else {
        panic!("a rect light is an area light");
    };
    let brighter = Arc::new(Emissive::light(Vec3A::new(5.0, 4.0, 3.0), None));
    // The same shape and geometry, a new emitter: what a rebuild hands back.
    let light = LightKind::Area(crust_core::AreaLight::new(
        old.shape().clone(),
        brighter.clone(),
        key.geoms.start,
    ));
    scene
        .lights
        .check_replace(key.index, &light)
        .expect("same kind and geometry");
    let slot: Arc<dyn Material> = brighter.clone();
    scene
        .world
        .check_materials(&[(&[key.geoms.start], &slot)])
        .expect("an emitter for an emitter");
    scene.lights.replace(key.index, light);
    scene
        .world
        .replace_materials(&[(&[key.geoms.start], &slot)]);

    let LightKind::Area(now) = &scene.lights.lights()[key.index] else {
        unreachable!()
    };
    let in_slot = &scene.world.materials()[key.geoms.start as usize];
    assert_eq!(
        ptr(in_slot),
        ptr(&(now.emitter().clone() as Arc<dyn Material>))
    );
    let (p, dir) = (Vec3A::ZERO, Vec3A::Y);
    assert_eq!(
        now.emitter().radiance_toward(p, dir, true),
        brighter.radiance_toward(p, dir, true)
    );
    assert_eq!(in_slot.emitted(), now.emitter().emitted());

    // A light at infinity cannot take an area light's place.
    let sky = scene
        .provenance
        .as_ref()
        .unwrap()
        .light("/World/Sky")
        .unwrap()
        .index;
    let area = scene.lights.lights()[key.index].clone();
    assert!(scene.lights.check_replace(sky, &area).is_err());
}

/// A stage rendering through a named camera, holding `shape`.
#[test]
fn light_indices_are_the_list_as_the_links_left_it() {
    let dir = std::env::temp_dir().join("crust_live_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let demoting = dir.join("live_demoted_light.usda");
    // `Dead` sits between two kept lights on either side, so whichever order
    // the traversal meets them in, two kept lights come after it.
    let light = |name: &str, x: f32, link: &str| {
        format!(
            r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.2
        float inputs:intensity = 10
        double3 xformOp:translate = ({x}, 3, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {link}
    }}
"#
        )
    };
    let nothing = "uniform bool collection:lightLink:includeRoot = 0";
    let lights: String = [
        light("K1", -2.0, ""),
        light("K2", -1.0, ""),
        light("Dead", 0.0, nothing),
        light("K3", 1.0, ""),
        light("K4", 2.0, ""),
    ]
    .concat();
    std::fs::write(
        &demoting,
        format!(
            r#"#usda 1.0
def Xform "W"
{{
{lights}
    def Mesh "Floor"
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
    }}
}}
"#
        ),
    )
    .expect("write stage");
    for path in [sample("light_linking.usda"), demoting.clone()] {
        let scene = Scene::from_usd(&path).expect("imports");
        let prov = consistent(&scene);
        let mut seen = 0;
        for (prim, origin) in prov.lights() {
            assert_eq!(
                scene.lights.name(origin.index),
                Some(prim),
                "{}: light {}",
                path.display(),
                origin.index
            );
            seen += 1;
        }
        assert_eq!(seen, scene.lights.count(), "{}", path.display());
    }
    let scene = Scene::from_usd(&demoting).expect("imports");
    let prov = scene.provenance.as_ref().unwrap();
    assert_eq!(scene.lights.count(), 4, "Dead illuminates nothing");
    assert!(prov.light("/W/Dead").is_none());
}

/// What a geometry's table was decoded for is part of what a swap must keep:
/// a material that starts reading a primvar is refused on a curves slot (its
/// strand table lacks the primvar) and on a mesh's (its primvar table does).
#[test]
fn a_material_swap_is_refused_when_it_reads_a_new_primvar() {
    let dir = std::env::temp_dir().join("crust_live_curve_swap");
    std::fs::create_dir_all(&dir).unwrap();
    let mtlx = dir.join("looks.mtlx");
    std::fs::write(
        &mtlx,
        r#"<?xml version="1.0"?>
<materialx version="1.39">
  <constant name="grey" type="color3"><input name="value" type="color3" value="0.5, 0.5, 0.5" /></constant>
  <constant name="red" type="color3"><input name="value" type="color3" value="0.9, 0.1, 0.1" /></constant>
  <geompropvalue name="tint" type="color3"><input name="geomprop" type="string" value="tint" /></geompropvalue>
  <standard_surface name="s_plain" type="surfaceshader"><input name="base_color" type="color3" nodename="grey" /></standard_surface>
  <standard_surface name="s_red" type="surfaceshader"><input name="base_color" type="color3" nodename="red" /></standard_surface>
  <standard_surface name="s_reads" type="surfaceshader"><input name="base_color" type="color3" nodename="tint" /></standard_surface>
  <surfacematerial name="plain" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_plain" /></surfacematerial>
  <surfacematerial name="red" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_red" /></surfacematerial>
  <surfacematerial name="reads" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_reads" /></surfacematerial>
</materialx>
"#,
    )
    .unwrap();
    let stage = dir.join("stage.usda");
    std::fs::write(
        &stage,
        format!(
            r#"#usda 1.0
def Xform "World"
{{
    def Material "Plain" (prepend references = @{}@</MaterialX/Materials/plain>) {{ }}
    def BasisCurves "Curve" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(0, 0, 0), (1, 0, 0)]
        rel material:binding = </World/Plain>
    }}
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 2, 0), (1, 2, 0), (0, 3, 0)]
        rel material:binding = </World/Plain>
    }}
}}
"#,
            mtlx.display()
        ),
    )
    .unwrap();
    let scene = Scene::from_usd(&stage).expect("imports");
    let prov = consistent(&scene);
    let slot = |prim: &str| {
        let slots = &prov.material("/World/Plain").expect("bound").slots;
        *slots
            .iter()
            .find(|&&s| prov.prim_of_geometry(s) == Some(prim))
            .unwrap_or_else(|| panic!("{prim}'s slot"))
    };
    let (curve, card) = (slot("/World/Curve"), slot("/World/Card"));
    let load = |name: &str| -> Arc<dyn Material> {
        crust_core::materialx::load(
            &mtlx,
            Some(name),
            &crust_core::mtlx::Host::new(&|_, _| None),
        )
        .unwrap_or_else(|e| panic!("{name}: {e:?}"))
        .material
    };
    assert!(swap_check(&scene, &[curve], load("red")), "same needs");
    assert!(
        !swap_check(&scene, &[curve], load("reads")),
        "a new primvar"
    );
    assert!(
        !swap_check(&scene, &[card], load("reads")),
        "a new mesh primvar"
    );
    assert!(
        swap_check(&scene, &[card], load("red")),
        "same needs on a mesh"
    );
}

/// A stage for the pairs: a camera, two meshes, each bound to a preview
/// material, a dome and a sphere light, the settings at 32x18; `settings`,
/// `camera` and `world` go into the settings prim, the camera and `/World`.
fn scene_text(settings: &str, camera: &str, world: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)
def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        float horizontalAperture = 24
        float verticalAperture = 13.5
        double3 xformOp:translate = (0, 1, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
{camera}
    }}
    def Mesh "A" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-3, 0, -1), (-0.5, 0, -1), (-0.5, 2, -1), (-3, 2, -1)]
        rel material:binding = </World/Looks/Red>
        double3 xformOp:translate = (0, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Mesh "B" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0.5, 0, -1), (3, 0, -1), (3, 2, -1), (0.5, 2, -1)]
        rel material:binding = </World/Looks/Grey>
    }}
    def Scope "Looks"
    {{
        def Material "Red"
        {{
            token outputs:surface.connect = </World/Looks/Red/S.outputs:surface>
            def Shader "S"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }}
        }}
        def Material "Grey"
        {{
            token outputs:surface.connect = </World/Looks/Grey/S.outputs:surface>
            def Shader "S"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
                token outputs:surface
            }}
        }}
    }}
    def DomeLight "Sky"
    {{
        float inputs:intensity = 0.3
    }}
    def SphereLight "Key"
    {{
        float inputs:radius = 0.3
        float inputs:intensity = 30
        double3 xformOp:translate = (0, 3, 2)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
{world}
}}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        int2 resolution = (32, 18)
{settings}
    }}
}}
"#
    )
}

/// Opens `text` as `name`, makes `edit`, updates, and checks the class
/// and that the render is the one a fresh delegate over the edited stage
/// renders, bit for bit.
fn pair(name: &str, text: &str, class: UpdateClass, edit: impl FnOnce(&mut Host)) -> Host {
    let path = write(name, text);
    let mut host = Host::open(&path);
    edit(&mut host);
    assert_eq!(host.update(), class, "{name}");
    let fresh = host.fresh();
    let warnings = |s: &crust_core::Scene| -> Vec<&'static str> {
        s.warnings.iter().map(|w| w.code.as_str()).collect()
    };
    assert_eq!(
        warnings(host.scene()),
        warnings(&fresh),
        "{name}: the warnings"
    );
    assert_eq!(host.scene().time, fresh.time, "{name}: the time");
    let updated = host.take();
    assert!(bits(updated) == bits(fresh), "{name}: the render");
    // Put a committed scene back for a caller that goes on.
    host
}

/// render-delegate spec, "A shader input".
#[test]
fn a_shader_input_is_a_materials_update() {
    pair(
        "shader_input.usda",
        &scene_text("", "", ""),
        UpdateClass::Materials,
        |h| {
            h.set(
                "/World/Looks/Red/S.inputs:diffuseColor",
                moho::openusd::gf::Vec3f {
                    x: 0.1,
                    y: 0.8,
                    z: 0.1,
                },
            )
        },
    );
}

/// render-delegate spec, "Adding a light with trivial collections".
#[test]
fn a_light_added_with_trivial_collections_is_a_lights_update() {
    pair(
        "light_added.usda",
        &scene_text("", "", ""),
        UpdateClass::Lights,
        |h| {
            let light = h
                .stage
                .define_typed_prim("/World/Fill", "SphereLight")
                .unwrap();
            light
                .create_attribute("inputs:intensity", "float")
                .unwrap()
                .set(20.0f32)
                .unwrap();
        },
    );
}

/// render-delegate spec, "Adding a light linked to one prim".
#[test]
fn a_light_linked_to_one_prim_added_is_a_lights_update() {
    let host = pair(
        "linked_light_added.usda",
        &scene_text("", "", ""),
        UpdateClass::Lights,
        |h| {
            let light = h
                .stage
                .define_typed_prim("/World/Spot", "RectLight")
                .unwrap();
            light
                .create_attribute("inputs:intensity", "float")
                .unwrap()
                .set(40.0f32)
                .unwrap();
            light.clone().apply_api("CollectionAPI:lightLink").unwrap();
            light
                .create_attribute("collection:lightLink:includeRoot", "bool")
                .unwrap()
                .set(false)
                .unwrap();
            light
                .create_relationship("collection:lightLink:includes")
                .unwrap()
                .set_targets(["/World/A"])
                .unwrap();
        },
    );
    drop(host);
}

/// render-delegate spec, "A mesh's transform".
#[test]
fn a_mesh_transform_is_a_geometry_update() {
    pair(
        "mesh_transform.usda",
        &scene_text("", "", ""),
        UpdateClass::Geometry,
        |h| {
            h.set(
                "/World/A.xformOp:translate",
                moho::openusd::gf::Vec3d {
                    x: 0.0,
                    y: 0.5,
                    z: 0.0,
                },
            )
        },
    );
}

/// Two meshes of one shape, `C` and `D`: a shared mesh, each placement an
/// instance of it, with authored normals.
const SHARED: &str = r#"    def Mesh "C" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 1, 0), (-1, 1, 0)]
        normal3f[] primvars:normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        rel material:binding = </World/Looks/Grey>
        double3 xformOp:translate = (-1.5, 2.5, -0.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Mesh "D" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 1, 0), (-1, 1, 0)]
        normal3f[] primvars:normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        rel material:binding = </World/Looks/Grey>
        double3 xformOp:translate = (1.5, 2.5, -0.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;

fn vec3f(x: f32, y: f32, z: f32) -> moho::openusd::gf::Vec3f {
    moho::openusd::gf::Vec3f { x, y, z }
}

/// render-delegate spec, "Reshaping a mesh another prim shares": one of two
/// placements of a shared mesh reshaped, alone or moved in the same batch,
/// takes a mesh of its own and bakes, and so does the other, left alone: in
/// place, bit for bit a fresh delegate's.
#[test]
fn points_and_normals_of_a_shared_mesh_are_reshaped_in_place() {
    let host = Host::open(&write("shared_probe.usda", &scene_text("", "", SHARED)));
    let prov = host.scene().provenance.as_ref().unwrap();
    assert!(
        prov.reshapable("/World/C").is_none(),
        "C's mesh is shared by direct placements only"
    );
    drop(host);
    let quad = |top: f32| {
        vec![
            vec3f(-1.0, 0.0, 0.0),
            vec3f(1.0, 0.0, 0.0),
            vec3f(1.0, top, 0.0),
            vec3f(-1.0, 1.0, 0.0),
        ]
    };
    chain(
        "shared_points.usda",
        &scene_text("", "", SHARED),
        UpdateClass::Geometry,
        &[
            Box::new(move |h| h.set("/World/C.points", quad(1.8))),
            // Then the other one, now baked alone, reshaped as a baked mesh.
            Box::new(move |h| h.set("/World/D.points", quad(1.4))),
        ],
    );
    // New normals alone leave the mesh's content key as it was: a fresh
    // commit still shares one mesh between the two, so the update is full.
    pair(
        "shared_normals.usda",
        &scene_text("", "", SHARED),
        UpdateClass::Full,
        |h| {
            let n = vec3f(0.0, 0.6, 0.8);
            h.set("/World/C.primvars:normals", vec![n, n, n, n])
        },
    );
    chain(
        "shared_points_and_move.usda",
        &scene_text("", "", SHARED),
        UpdateClass::Geometry,
        &[Box::new(move |h| {
            h.set(
                "/World/C.xformOp:translate",
                moho::openusd::gf::Vec3d {
                    x: -1.5,
                    y: 3.0,
                    z: -0.5,
                },
            );
            h.set("/World/C.points", quad(1.8));
        })],
    );
}

/// render-delegate spec, "A reshape that makes two meshes identical".
#[test]
fn a_reshape_that_makes_two_meshes_identical_is_full() {
    let e = r#"    def Mesh "E" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 2, 0), (-1, 1, 0)]
        normal3f[] primvars:normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        rel material:binding = </World/Looks/Grey>
        double3 xformOp:translate = (0, 4, -0.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;
    pair(
        "reshape_to_shared.usda",
        &scene_text("", "", &format!("{SHARED}\n{e}")),
        UpdateClass::Full,
        |h| {
            h.set(
                "/World/E.points",
                vec![
                    vec3f(-1.0, 0.0, 0.0),
                    vec3f(1.0, 0.0, 0.0),
                    vec3f(1.0, 1.0, 0.0),
                    vec3f(-1.0, 1.0, 0.0),
                ],
            )
        },
    );
}

/// A prototype mesh `/lib/shape` placed by two native instances, and a point
/// instancer placing its own prototype mesh twice.
const PROTOTYPES: &str = r#"
class Xform "lib"
{
    def Mesh "shape"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0.8, 0, 0), (0.8, 0.8, 0), (0, 0.8, 0)]
    }
}
def Xform "inst"
{
    def Xform "i0" (
        instanceable = true
        references = </lib>
    )
    {
        double3 xformOp:translate = (-2, 0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "i1" (
        instanceable = true
        references = </lib>
    )
    {
        double3 xformOp:translate = (1, 0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
def PointInstancer "pi"
{
    rel prototypes = [</pi/Protos/p>]
    int[] protoIndices = [0, 0]
    point3f[] positions = [(-1, 1.5, 0.5), (1, 1.5, 0.5)]
    def Scope "Protos"
    {
        def Mesh "p"
        {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, 0), (0.6, 0, 0), (0, 0.6, 0)]
        }
    }
}
"#;

/// render-delegate spec, "Reshaping a prototype's mesh": a native
/// instance's prototype mesh and a point instancer's, each reshaped in place
/// at every placement (the dirty reaches the delegate at the Hydra
/// prototype path: Moho gap G3 does not arise).
#[test]
fn a_prototype_mesh_is_reshaped_in_place() {
    let text = scene_text("", "", "") + PROTOTYPES;
    chain(
        "proto_native.usda",
        &text,
        UpdateClass::Geometry,
        &[Box::new(|h| {
            h.set(
                "/lib/shape.points",
                vec![
                    vec3f(0.0, 0.0, 0.0),
                    vec3f(0.8, 0.0, 0.0),
                    vec3f(0.8, 1.2, 0.0),
                    vec3f(0.0, 0.8, 0.0),
                ],
            )
        })],
    );
    chain(
        "proto_instancer.usda",
        &text,
        UpdateClass::Geometry,
        &[Box::new(|h| {
            h.set(
                "/pi/Protos/p.points",
                vec![
                    vec3f(0.0, 0.0, 0.0),
                    vec3f(0.6, 0.0, 0.3),
                    vec3f(0.0, 0.9, 0.0),
                ],
            )
        })],
    );
}

/// A baked mesh placed once reshapes in place, alone or moved in the same
/// batch.
#[test]
fn points_of_a_baked_mesh_are_a_geometry_update() {
    let points = vec![
        vec3f(-3.0, 0.0, -1.0),
        vec3f(-0.5, 0.0, -1.0),
        vec3f(-0.5, 2.5, -1.0),
        vec3f(-3.0, 2.0, -1.0),
    ];
    let p = points.clone();
    pair(
        "baked_points.usda",
        &scene_text("", "", ""),
        UpdateClass::Geometry,
        move |h| h.set("/World/A.points", p),
    );
    pair(
        "baked_points_and_move.usda",
        &scene_text("", "", ""),
        UpdateClass::Geometry,
        move |h| {
            h.set(
                "/World/A.xformOp:translate",
                moho::openusd::gf::Vec3d {
                    x: 0.0,
                    y: 0.5,
                    z: 0.0,
                },
            );
            h.set("/World/A.points", points);
        },
    );
}

/// An edit the index sends between `prepare` and `update` reaches that
/// update: it applies the prepared batch and everything flushed since.
#[test]
fn an_edit_after_prepare_reaches_the_update() {
    let path = write("after_prepare.usda", &scene_text("", "", ""));
    let mut host = Host::open(&path);
    host.set("/World/Key.inputs:intensity", 60.0f32);
    assert_eq!(host.prepare(), UpdateClass::Lights);
    host.set(
        "/World/Looks/Red/S.inputs:diffuseColor",
        vec3f(0.1, 0.8, 0.1),
    );
    // A light and a material: two in-place classes, so the batch is full.
    assert_eq!(
        host.update(),
        UpdateClass::Full,
        "the later edit is in the batch"
    );
    let fresh = host.fresh();
    assert!(
        bits(host.take()) == bits(fresh),
        "the render has both edits"
    );
}

/// An update that fails applies nothing and keeps its notices pending: the
/// next update still sees the edits, and renders them once it succeeds.
#[test]
fn a_failed_update_keeps_its_edits_pending() {
    let path = write("failed_update.usda", &scene_text("", "", ""));
    let mut host = Host::open_with(
        &path,
        crust_core::UsdImportOptions {
            camera: Some("/World/Cam".into()),
            ..Default::default()
        },
    );
    host.set("/World/Key.inputs:intensity", 60.0f32);
    host.stage
        .prim("/World/Cam")
        .unwrap()
        .set_active(false)
        .unwrap();
    assert!(
        host.try_update().is_err(),
        "the camera the host asked for is gone"
    );
    assert_ne!(
        host.prepare(),
        UpdateClass::Unchanged,
        "the failed update's notices are still pending"
    );
    host.stage
        .prim("/World/Cam")
        .unwrap()
        .set_active(true)
        .unwrap();
    assert_eq!(host.try_update().unwrap(), UpdateClass::Full);
    let fresh = host.fresh();
    assert!(
        bits(host.take()) == bits(fresh),
        "the retry renders the light edit"
    );
}

/// render-delegate spec, "Exposure is a settings edit".
#[test]
fn camera_exposure_is_a_settings_update() {
    pair(
        "exposure.usda",
        &scene_text("", "        float exposure = 0", ""),
        UpdateClass::Settings,
        |h| h.set("/World/Cam.exposure", 1.0f32),
    );
}

/// render-delegate spec, "Focal length on an adaptively subdivided scene".
#[test]
fn focal_length_under_adaptive_subdivision_is_full() {
    pair(
        "adaptive_focal.usda",
        &scene_text("        float crust:subdivisionEdgeLength = 4", "", ""),
        UpdateClass::Full,
        |h| h.set("/World/Cam.focalLength", 35.0f32),
    );
}

/// render-delegate spec, "A shutter change".
#[test]
fn a_shutter_change_on_a_motion_blurred_scene_is_full() {
    let world = r#"    def Sphere "Ball"
    {
        double radius = 0.4
        double3 xformOp:translate.timeSamples = { 0: (0, 1, 0), 1: (1, 1, 0) }
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;
    pair(
        "shutter.usda",
        &scene_text(
            "",
            "        double shutter:open = 0\n        double shutter:close = 0.5",
            world,
        ),
        UpdateClass::Full,
        |h| h.set("/World/Cam.shutter:close", 0.25f64),
    );
}

/// The settings class's other `full` guards (render-delegate spec,
/// "Updates apply in place from the dirty locators").
#[test]
fn settings_edits_that_move_the_scene_are_full() {
    // The subdivision level.
    pair(
        "subdiv_level.usda",
        &scene_text("        int crust:subdivisionLevel = 0", "", ""),
        UpdateClass::Full,
        |h| h.set("/Render/settings.crust:subdivisionLevel", 2i32),
    );
    // The working colour space.
    pair(
        "working_space.usda",
        &scene_text("        token renderingColorSpace = \"lin_rec709\"", "", ""),
        UpdateClass::Full,
        |h| {
            h.set(
                "/Render/settings.renderingColorSpace",
                moho::openusd::tf::Token::from("acescg"),
            )
        },
    );
    // The dome's camera visibility.
    pair(
        "dome_visibility.usda",
        &scene_text("        bool crust:domeLightCameraVisibility = 1", "", ""),
        UpdateClass::Full,
        |h| h.set("/Render/settings.crust:domeLightCameraVisibility", false),
    );
    // The dicing projection: the dicing camera moves under adaptive
    // subdivision.
    pair(
        "dicing.usda",
        &scene_text(
            "        float crust:subdivisionEdgeLength = 4\n        string crust:dicingCamera = \"/World/Cam\"",
            "",
            "",
        ),
        UpdateClass::Full,
        |h| {
            h.set(
                "/World/Cam.xformOp:translate",
                moho::openusd::gf::Vec3d {
                    x: 0.0,
                    y: 1.0,
                    z: 6.0,
                },
            )
        },
    );
}

/// A precomp before or after the edit, and the primvars the products keep:
/// `full`.
#[test]
fn a_precomp_or_new_kept_primvars_are_full() {
    pair(
        "precomp.usda",
        &scene_text("        token crust:display = \"\"", "", ""),
        UpdateClass::Full,
        |h| {
            h.set(
                "/Render/settings.crust:display",
                moho::openusd::tf::Token::from("beauty"),
            )
        },
    );
    let products = r#"        rel products = [</Render/P>]"#;
    let text = scene_text(products, "", "")
        + r#"
over "Render"
{
    def RenderProduct "P"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/V>]
    }
    def RenderVar "V"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "color"
    }
}
"#;
    pair("kept_primvars.usda", &text, UpdateClass::Full, |h| {
        h.set("/Render/V.sourceName", "tint".to_owned());
        h.set(
            "/Render/V.sourceType",
            moho::openusd::tf::Token::from("primvar"),
        );
    });
}

/// render-delegate spec, "A prim added".
#[test]
fn a_prim_added_is_full_and_renders() {
    let host = pair(
        "prim_added.usda",
        &scene_text("", "", ""),
        UpdateClass::Full,
        |h| {
            let mesh = h.stage.define_typed_prim("/World/C", "Mesh").unwrap();
            mesh.create_attribute("faceVertexCounts", "int[]")
                .unwrap()
                .set(vec![4i32])
                .unwrap();
            mesh.create_attribute("faceVertexIndices", "int[]")
                .unwrap()
                .set(vec![0i32, 1, 2, 3])
                .unwrap();
            mesh.create_attribute("points", "point3f[]")
                .unwrap()
                .set(vec![
                    moho::openusd::gf::Vec3f {
                        x: -1.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    moho::openusd::gf::Vec3f {
                        x: 1.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    moho::openusd::gf::Vec3f {
                        x: 1.0,
                        y: 1.0,
                        z: 0.0,
                    },
                    moho::openusd::gf::Vec3f {
                        x: -1.0,
                        y: 1.0,
                        z: 0.0,
                    },
                ])
                .unwrap();
        },
    );
    drop(host);
}

/// render-delegate spec, "A camera that moves over time".
#[test]
fn a_time_change_moving_only_the_camera_is_a_settings_update() {
    let camera =
        r#"        double3 xformOp:translate.timeSamples = { 0: (0, 1, 8), 10: (1, 1, 8) }"#;
    let text = scene_text("", "", "").replace(
        "        double3 xformOp:translate = (0, 1, 8)\n",
        &format!("{camera}\n"),
    );
    let host = pair("camera_time.usda", &text, UpdateClass::Settings, |h| {
        h.set_time(5.0)
    });
    drop(host);
}

/// Whether the host's scene was last brought up to date in place as
/// `class`, as its stats record it.
fn in_place(host: &Host, class: &str) -> bool {
    let name = format!("Update in place ({class})");
    host.scene().stats.phases.iter().any(|p| p.name == name)
}

/// One host edit, then an update.
type Step = Box<dyn Fn(&mut Host)>;

/// For each prefix of `steps`: opens `text` as `name`, makes the steps,
/// updating after each and checking that every update reports `class` in
/// place, then checks that the scene renders as a fresh delegate's, bit for
/// bit, with the same warnings.
fn chain(name: &str, text: &str, class: UpdateClass, steps: &[Step]) {
    for k in 1..=steps.len() {
        let path = write(name, text);
        let mut host = Host::open(&path);
        for (i, step) in steps[..k].iter().enumerate() {
            step(&mut host);
            assert_eq!(host.update(), class, "{name}: step {}", i + 1);
            assert!(
                in_place(&host, class.as_str()),
                "{name}: step {} was not applied in place",
                i + 1
            );
        }
        let fresh = host.fresh();
        let warnings = |s: &crust_core::Scene| -> Vec<&'static str> {
            s.warnings.iter().map(|w| w.code.as_str()).collect()
        };
        assert_eq!(
            warnings(host.scene()),
            warnings(&fresh),
            "{name}: the warnings"
        );
        assert_eq!(
            host.scene().lights.count(),
            fresh.lights.count(),
            "{name}: after {k} step(s), the lights"
        );
        let names = |s: &crust_core::Scene| -> Vec<Option<String>> {
            (0..s.lights.count())
                .map(|i| s.lights.name(i).map(str::to_owned))
                .collect()
        };
        assert_eq!(
            names(host.scene()),
            names(&fresh),
            "{name}: the list's order"
        );
        assert!(
            bits(host.take()) == bits(fresh),
            "{name}: after {k} step(s), the render"
        );
    }
}

/// Defines a light of type `ty` at `path`, `intensity` strong, translated to
/// `at`, with `collection:<link>:includeRoot` and `:includes` / `:excludes`
/// as `link` gives them.
fn light(
    h: &mut Host,
    path: &str,
    ty: &str,
    intensity: f32,
    at: (f64, f64, f64),
    link: Option<(&str, bool, &str, &[&str])>,
) {
    let light = h.stage.define_typed_prim(path, ty).unwrap();
    light
        .create_attribute("inputs:intensity", "float")
        .unwrap()
        .set(intensity)
        .unwrap();
    light
        .create_attribute("xformOp:translate", "double3")
        .unwrap()
        .set(moho::openusd::gf::Vec3d {
            x: at.0,
            y: at.1,
            z: at.2,
        })
        .unwrap();
    light
        .create_attribute("xformOpOrder", "token[]")
        .unwrap()
        .set(vec![moho::openusd::tf::Token::from("xformOp:translate")])
        .unwrap();
    if let Some((collection, include_root, rel, targets)) = link {
        light
            .clone()
            .apply_api(format!("CollectionAPI:{collection}"))
            .unwrap();
        light
            .create_attribute(format!("collection:{collection}:includeRoot"), "bool")
            .unwrap()
            .set(include_root)
            .unwrap();
        light
            .create_relationship(format!("collection:{collection}:{rel}"))
            .unwrap()
            .set_targets(targets.iter().copied())
            .unwrap();
    }
}

/// Sets the targets of `rel` (`/Prim.collection:…:includes`).
fn targets(h: &Host, rel: &str, targets: &[&str]) {
    h.stage
        .create_relationship(rel)
        .unwrap()
        .set_targets(targets.iter().copied())
        .unwrap();
}

/// A rect light `/World/Rim`, before `/World/Sky` and the sphere `Key` in
/// path order, and a shadow-linked sphere light `/World/Shadow` excluding
/// `/World/B` from its shadows; `/World/Dead`, a rect light whose lightLink
/// includes nothing, illuminating nothing.
const LIGHTS: &str = r#"    def RectLight "Rim"
    {
        float inputs:width = 0.6
        float inputs:height = 0.6
        float inputs:intensity = 20
        bool crust:light:cameraVisible = 1
        double3 xformOp:translate = (-1, 2, 1)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def SphereLight "Shadow"
    {
        float inputs:radius = 0.2
        float inputs:intensity = 25
        double3 xformOp:translate = (2, 3, 1)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        prepend rel collection:shadowLink:excludes = </World/B>
    }
    def RectLight "Dead"
    {
        float inputs:width = 0.5
        float inputs:height = 0.5
        float inputs:intensity = 30
        bool crust:light:cameraVisible = 1
        double3 xformOp:translate = (0, 0.5, 1.5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        uniform bool collection:lightLink:includeRoot = 0
    }"#;

/// render-delegate spec, "Adding a light with trivial collections", "Adding
/// a light linked to one prim", "Adding a shadow-linked light among
/// shadow-linked lights": each in place, bitwise a fresh delegate's.
#[test]
fn lights_added_are_applied_in_place() {
    let text = scene_text("", "", LIGHTS);
    chain(
        "layer_trivial.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            light(
                h,
                "/World/Fill",
                "SphereLight",
                20.0,
                (-2.0, 3.0, 2.0),
                None,
            )
        })],
    );
    chain(
        "layer_linked.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            light(
                h,
                "/World/Spot",
                "RectLight",
                40.0,
                (0.0, 3.0, 1.0),
                Some(("lightLink", false, "includes", &["/World/A"])),
            )
        })],
    );
    chain(
        "layer_shadow_linked.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            light(
                h,
                "/World/Accent",
                "SphereLight",
                30.0,
                (-2.0, 3.0, 1.0),
                Some(("shadowLink", true, "excludes", &["/World/A"])),
            )
        })],
    );
    // Linked to nothing: demoted as it is added.
    chain(
        "layer_linked_to_nothing.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            light(
                h,
                "/World/Nobody",
                "RectLight",
                40.0,
                (1.0, 1.0, 1.0),
                Some(("lightLink", false, "includes", &[])),
            )
        })],
    );
}

/// render-delegate spec, "Removing a light": an area light before another in
/// path order, and the dome.
#[test]
fn lights_removed_are_applied_in_place() {
    let text = scene_text("", "", LIGHTS);
    chain(
        "layer_removed_area.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            h.stage.remove_prim("/World/Rim").unwrap();
        })],
    );
    chain(
        "layer_removed_dome.usda",
        &text,
        UpdateClass::Lights,
        &[Box::new(|h| {
            h.stage.remove_prim("/World/Sky").unwrap();
        })],
    );
}

/// render-delegate spec, "A receiver's categories" and "A light linked to
/// nothing comes back": a mesh included in a light's lightLink; `Dead`, linked
/// to nothing at the commit, given a receiver and so in the list again at its
/// path-order index, then demoted again by the reverse edit.
#[test]
fn categories_are_applied_in_place() {
    let text = scene_text("", "", LIGHTS);
    let linked = format!(
        "{LIGHTS}\n{}",
        r#"    def RectLight "Spot"
    {
        float inputs:width = 0.5
        float inputs:height = 0.5
        float inputs:intensity = 40
        double3 xformOp:translate = (0, 3, 1)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        uniform bool collection:lightLink:includeRoot = 0
        prepend rel collection:lightLink:includes = </World/A>
    }"#
    );
    chain(
        "layer_categories.usda",
        &scene_text("", "", &linked),
        UpdateClass::Lights,
        &[Box::new(|h| {
            targets(
                h,
                "/World/Spot.collection:lightLink:includes",
                &["/World/A", "/World/B"],
            )
        })],
    );
    chain(
        "layer_revived.usda",
        &text,
        UpdateClass::Lights,
        &[
            Box::new(|h| {
                targets(
                    h,
                    "/World/Dead.collection:lightLink:includes",
                    &["/World/B"],
                )
            }),
            Box::new(|h| targets(h, "/World/Dead.collection:lightLink:includes", &[])),
        ],
    );
}

/// An area light moved rebuilds the light layer alone: a geometry update,
/// in place.
#[test]
fn an_area_light_moved_is_applied_in_place() {
    chain(
        "layer_area_moved.usda",
        &scene_text("", "", LIGHTS),
        UpdateClass::Geometry,
        &[Box::new(|h| {
            h.set(
                "/World/Rim.xformOp:translate",
                moho::openusd::gf::Vec3d {
                    x: -0.5,
                    y: 2.2,
                    z: 1.0,
                },
            )
        })],
    );
}

/// usd-scene-import spec, "Emitters are numbered after the other geometry".
#[test]
fn emitters_are_numbered_after_the_other_geometry() {
    let path = write(
        "emitters_last.usda",
        r#"#usda 1.0
def RectLight "A"
{
    float inputs:intensity = 10
    bool crust:light:cameraVisible = 1
}
def Xform "B"
{
    def Mesh "Mesh"
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, -2), (1, 0, -2), (0, 1, -2)]
    }
}
"#,
    );
    let host = Host::open(&path);
    let prov = host.scene().provenance.as_ref().unwrap();
    let light = prov.light("/A").expect("the rect light").geoms.clone();
    let mesh = prov.geometry_of("/B/Mesh");
    assert_eq!(mesh.len(), 1);
    assert!(mesh[0].end <= light.start, "{mesh:?} before {light:?}");
    assert_eq!(host.scene().world.light_base(), light.start);
}

use moho::openusd::sdf;
use moho::scene_index::{Observer, Prim, SceneIndex, SceneIndexHandle};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A scene index recording the prims read through it while armed.
struct Counting {
    input: SceneIndexHandle,
    armed: Rc<Cell<bool>>,
    reads: Rc<RefCell<Vec<sdf::Path>>>,
}

impl SceneIndex for Counting {
    fn prim(&self, path: &sdf::Path) -> Prim {
        if self.armed.get() {
            self.reads.borrow_mut().push(path.clone());
        }
        self.input.prim(path)
    }
    fn children(&self, path: &sdf::Path) -> Vec<sdf::Path> {
        self.input.children(path)
    }
    fn add_observer(&self, observer: Weak<dyn Observer>) {
        self.input.add_observer(observer);
    }
}

/// render-delegate spec, "The prims a camera edit reads": a camera moved on a
/// scene of 1 000 meshes reads no mesh, material or light prim.
#[test]
fn a_camera_edit_reads_no_mesh_material_or_light() {
    let mut world = String::new();
    for i in 0..1000 {
        let x = (i % 40) as f32 * 0.1 - 2.0;
        let y = (i / 40) as f32 * 0.1;
        world.push_str(&format!(
            r#"    def Mesh "M{i}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [({x}, {y}, -2), ({}, {y}, -2), ({x}, {}, -2)]
        rel material:binding = </World/Looks/{}>
    }}
"#,
            x + 0.05,
            y + 0.05,
            if i % 2 == 0 { "Red" } else { "Grey" }
        ));
    }
    let path = write("counted_camera.usda", &scene_text("", "", &world));
    let armed = Rc::new(Cell::new(false));
    let reads = Rc::new(RefCell::new(Vec::new()));
    let (a, r) = (armed.clone(), reads.clone());
    let mut host = Host::open_wrapped(
        &path,
        crust_core::UsdImportOptions::default(),
        Some(Box::new(move |input| {
            Rc::new(Counting {
                input,
                armed: a.clone(),
                reads: r.clone(),
            }) as SceneIndexHandle
        })),
    );
    host.set(
        "/World/Cam.xformOp:translate",
        moho::openusd::gf::Vec3d {
            x: 0.1,
            y: 1.0,
            z: 8.0,
        },
    );
    host.prepare();
    armed.set(true);
    assert_eq!(host.update(), UpdateClass::Settings);
    armed.set(false);
    let reads = reads.borrow();
    assert!(!reads.is_empty(), "the update read the settings");
    let scene_prim = |p: &sdf::Path| {
        let p = p.as_str();
        p.starts_with("/World/M")
            || p == "/World/A"
            || p == "/World/B"
            || p.starts_with("/World/Looks")
            || p == "/World/Sky"
            || p == "/World/Key"
    };
    let wrong: Vec<&sdf::Path> = reads.iter().filter(|p| scene_prim(p)).collect();
    assert!(
        wrong.is_empty(),
        "a settings update read {} scene prim(s): {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)]
    );
}

/// A scene index that, armed with a weak reference, records whether it is
/// dead at the first mesh prim read through it: when a fresh commit starts
/// reading geometry.
struct Watching {
    input: SceneIndexHandle,
    watch: Rc<RefCell<Option<std::sync::Weak<dyn crust_core::Material>>>>,
    dead: Rc<Cell<Option<bool>>>,
}

impl SceneIndex for Watching {
    fn prim(&self, path: &sdf::Path) -> Prim {
        let prim = self.input.prim(path);
        if prim.ty.as_str() == "mesh"
            && let Some(weak) = self.watch.borrow_mut().take()
        {
            self.dead.set(Some(weak.upgrade().is_none()));
        }
        prim
    }
    fn children(&self, path: &sdf::Path) -> Vec<sdf::Path> {
        self.input.children(path)
    }
    fn add_observer(&self, observer: Weak<dyn Observer>) {
        self.input.add_observer(observer);
    }
}

/// render-delegate spec, "A re-commit holds one scene": on each path that
/// commits afresh, the old world is gone before the fresh commit reads its
/// first mesh.
#[test]
fn a_recommit_holds_one_scene() {
    let pass = r#"
def RenderPass "Pass"
{
}
"#;
    let cases: [(&str, String, Option<&str>, Step); 3] = [
        (
            "one_scene_full.usda",
            scene_text("", "", ""),
            None,
            Box::new(|h| {
                let mesh = h.stage.define_typed_prim("/World/C", "Mesh").unwrap();
                mesh.create_attribute("faceVertexCounts", "int[]")
                    .unwrap()
                    .set(vec![3i32])
                    .unwrap();
                mesh.create_attribute("faceVertexIndices", "int[]")
                    .unwrap()
                    .set(vec![0i32, 1, 2])
                    .unwrap();
                mesh.create_attribute("points", "point3f[]")
                    .unwrap()
                    .set(vec![
                        vec3f(0.0, 0.0, -1.0),
                        vec3f(1.0, 0.0, -1.0),
                        vec3f(0.0, 1.0, -1.0),
                    ])
                    .unwrap();
            }),
        ),
        (
            "one_scene_pass.usda",
            scene_text("", "", "") + pass,
            Some("/Pass"),
            Box::new(|h| light(h, "/World/Fill", "SphereLight", 20.0, (1.0, 3.0, 2.0), None)),
        ),
        (
            "one_scene_dicing.usda",
            scene_text(
                "        float crust:subdivisionEdgeLength = 4\n        string crust:dicingCamera = \"/World/Cam\"",
                "",
                "",
            ),
            None,
            Box::new(|h| h.set("/World/Cam.focalLength", 35.0f32)),
        ),
    ];
    for (name, text, pass, edit) in cases {
        let watch = Rc::new(RefCell::new(None));
        let dead = Rc::new(Cell::new(None));
        let (w, d) = (watch.clone(), dead.clone());
        let mut host = Host::open_wrapped(
            &write(name, &text),
            crust_core::UsdImportOptions {
                pass: pass.map(str::to_owned),
                ..Default::default()
            },
            Some(Box::new(move |input| {
                Rc::new(Watching {
                    input,
                    watch: w.clone(),
                    dead: d.clone(),
                }) as SceneIndexHandle
            })),
        );
        edit(&mut host);
        host.prepare();
        {
            let scene = host.scene();
            let slot = scene
                .provenance
                .as_ref()
                .unwrap()
                .material("/World/Looks/Red")
                .expect("A's material")
                .slots[0];
            let material: Arc<dyn Material> = scene.world.materials()[slot as usize].clone();
            *watch.borrow_mut() = Some(Arc::downgrade(&material));
        }
        assert_eq!(host.update(), UpdateClass::Full, "{name}");
        assert_eq!(
            dead.get(),
            Some(true),
            "{name}: the old world was let go before the fresh commit read a mesh"
        );
    }
}
