//! The live session over an owned layer stack (`live-session-host`): a root
//! the host writes, sublayers it owns beside it, a sample beneath them.

use super::live::{Error, LiveSession, Pick};
use crate::session::delta::Applied;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `samples/<name>`, absolute, with forward slashes as a USD asset path.
fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    path.canonicalize()
        .expect("the sample exists")
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

fn work_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_live_session").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test directory");
    dir
}

/// Writes `path` and moves its modification time a second past the old one,
/// so a change is seen on a filesystem whose clock is coarse.
fn write(path: &Path, text: &str) {
    let before = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    std::fs::write(path, text).expect("the layer is written");
    if let Some(before) = before {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(before + Duration::from_secs(1)).unwrap();
    }
}

/// A root over `sublayers` (relative names) and the Cornell box, with small
/// settings of its own.
fn write_root(dir: &Path, sublayers: &[&str]) -> PathBuf {
    let mut subs: Vec<String> = sublayers.iter().map(|s| format!("@./{s}@")).collect();
    subs.push(format!("@{}@", sample("cornellbox.usda")));
    let root = dir.join("root.usda");
    write(
        &root,
        &format!(
            "#usda 1.0\n(\n    subLayers = [{}]\n)\n\ndef Scope \"Render\"\n{{\n    \
             def RenderSettings \"settings\"\n    {{\n        int2 resolution = (24, 18)\n        \
             int crust:samplesPerPixel = 4\n    }}\n}}\n",
            subs.join(", ")
        ),
    );
    root
}

fn graph_layer(body: &str) -> String {
    format!("#usda 1.0\n\nover \"scene\"\n{{\n{body}\n}}\n")
}

fn sky(intensity: f32) -> String {
    format!("    over \"Sky\"\n    {{\n        float inputs:intensity = {intensity}\n    }}")
}

/// Waits for the render to complete and returns its final image.
fn converged(session: &LiveSession) -> Vec<f32> {
    let started = Instant::now();
    while !session.converged() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the render did not converge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let snapshot = session.snapshot().expect("a converged render");
    assert!(snapshot.completed);
    snapshot.rgba
}

/// The image a fresh session gives of the same files.
fn fresh(root: &Path, owned: &[PathBuf]) -> Vec<f32> {
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(root, owned).expect("the layers import");
    converged(&session)
}

#[test]
fn a_light_edit_in_an_owned_sublayer_applies_in_place_as_a_fresh_import() {
    let dir = work_dir("light_in_sublayer");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda"]);
    let owned = [graph.clone()];
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, &owned)
        .expect("the layers import");
    assert_eq!(session.applied(), Some(Applied::Full));
    let first = converged(&session);
    assert_eq!(session.render(), 1);

    write(&graph, &graph_layer(&sky(2.5)));
    session.apply().expect("the edit applies");
    assert_eq!(session.applied(), Some(Applied::Lights));
    assert_eq!(session.render(), 2, "an apply restarts the render");
    let edited = converged(&session);
    assert_ne!(first, edited, "the edit shows");
    assert!(
        edited == fresh(&root, &owned),
        "in place renders as a fresh import"
    );
}

#[test]
fn an_edit_in_the_root_and_in_a_sublayer_classify_together() {
    let dir = work_dir("root_and_sublayer");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    // A setting in the root, a light in the sublayer: the costlier class.
    let text = std::fs::read_to_string(&root).unwrap();
    write(
        &root,
        &text.replace("crust:samplesPerPixel = 4", "crust:samplesPerPixel = 2"),
    );
    write(&graph, &graph_layer(&sky(3.0)));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Lights));
    assert!(converged(&session) == fresh(&root, &[graph]));
}

#[test]
fn a_change_to_a_sublayer_the_session_does_not_own_is_full() {
    let dir = work_dir("foreign_sublayer");
    let (graph, other) = (dir.join("graph.usda"), dir.join("other.usda"));
    write(&graph, &graph_layer(&sky(1.0)));
    write(&other, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda", "other.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    // The same light edit, in a layer the session only watches.
    write(&other, &graph_layer(&sky(2.0)));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    // The owned layer is weaker here only by order: `graph` is stronger.
    assert!(converged(&session) == fresh(&root, &[graph]));
}

#[test]
fn an_owned_layer_outside_the_root_layer_stack_is_refused() {
    let dir = work_dir("outside_stack");
    let (graph, stray) = (dir.join("graph.usda"), dir.join("stray.usda"));
    write(&graph, &graph_layer(""));
    write(&stray, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    let refused = session.load_owned(&root, &[stray]);
    assert!(
        matches!(refused, Err(Error::InvalidArgument(_))),
        "{refused:?}"
    );
}

#[test]
fn cancel_render_stops_the_render_without_waiting_for_the_session_thread() {
    let dir = work_dir("cancel_render");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda"]);
    let text = std::fs::read_to_string(&root).unwrap();
    write(
        &root,
        &text
            .replace("(24, 18)", "(160, 120)")
            .replace("crust:samplesPerPixel = 4", "crust:samplesPerPixel = 4096"),
    );
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(&root, &[graph]).expect("imports");
    let started = Instant::now();
    while session.generation().is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "no first image"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let asked = Instant::now();
    session.cancel_render();
    while session.outcome().is_none() {
        assert!(
            asked.elapsed() < Duration::from_secs(10),
            "the render did not stop"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    assert_eq!(
        session.outcome(),
        Some(crust_core::RenderOutcome::Cancelled)
    );
    assert!(!session.converged());
}

/// Loads `root` owning `graph`, whose body starts as `before`, converges,
/// rewrites the body as `after`, applies, and checks the class and that the
/// render is bitwise a fresh session's of the same files.
fn edit_applies_as(test: &str, before: &str, after: &str, class: Applied) {
    let dir = work_dir(test);
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(before));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    let first = converged(&session);
    write(&graph, &graph_layer(after));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(class), "{test}");
    let edited = converged(&session);
    assert_ne!(first, edited, "{test}: the edit shows");
    assert!(
        edited == fresh(&root, &[graph]),
        "{test}: as a fresh import"
    );
}

fn over_prim(prim: &str, body: &str) -> String {
    format!("    over \"{prim}\"\n    {{\n        {body}\n    }}")
}

/// pCube2 is placed once: baked into the top level.
#[test]
fn moving_a_baked_mesh_applies_as_geometry() {
    edit_applies_as(
        "move_baked",
        "",
        &over_prim("pCube2", "double3 xformOp:translate = (0.5, 0.5, 0)"),
        Applied::Geometry,
    );
}

/// pSphere1 and pSphere2 are one mesh placed twice: instanced.
#[test]
fn moving_an_instanced_mesh_applies_as_geometry() {
    edit_applies_as(
        "move_instanced",
        "",
        &over_prim(
            "pSphere1",
            "double3 xformOp:translate = (-0.8, 0.4, -0.2)\n        float3 xformOp:scale = (0.25, 0.3, 0.25)",
        ),
        Applied::Geometry,
    );
}

/// A mirroring scale flips a baked mesh's winding.
#[test]
fn mirroring_a_baked_mesh_applies_as_geometry() {
    edit_applies_as(
        "mirror_baked",
        "",
        &over_prim("pTorus1", "float3 xformOp:scale = (-0.25, 0.25, 0.25)"),
        Applied::Geometry,
    );
}

/// `prim`'s authored points in the Cornell box, every coordinate times `s`.
fn scaled_points(prim: &str, s: f32) -> String {
    let text = std::fs::read_to_string(sample("cornellbox.usda")).unwrap();
    let at = text.find(&format!("def Mesh \"{prim}\"")).unwrap();
    let start = text[at..].find("point3f[] points = ").unwrap() + at;
    let start = start + "point3f[] points = ".len();
    let end = text[start..].find('\n').unwrap() + start;
    let mut out = String::from("point3f[] points = ");
    let mut number = String::new();
    let flush = |number: &mut String, out: &mut String| {
        if !number.is_empty() {
            let v: f32 = number.parse().unwrap();
            out.push_str(&format!("{}", v * s));
            number.clear();
        }
    };
    for c in text[start..end].chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' {
            number.push(c);
        } else {
            flush(&mut number, &mut out);
            out.push(c);
        }
    }
    flush(&mut number, &mut out);
    out
}

/// The torus is placed once: its points can be read again.
#[test]
fn new_points_on_a_baked_mesh_apply_as_geometry() {
    edit_applies_as(
        "reshape_baked",
        "",
        &over_prim("pTorus1", &scaled_points("pTorus1", 1.15)),
        Applied::Geometry,
    );
}

/// Reshaping one of two placements of a mesh splits it: a fresh import
/// would bake both.
#[test]
fn new_points_on_an_instanced_mesh_are_full() {
    edit_applies_as(
        "reshape_instanced",
        "",
        &over_prim("pSphere1", &scaled_points("pSphere1", 1.2)),
        Applied::Full,
    );
}

/// A reshaped mesh that becomes another's would share it in a fresh import.
#[test]
fn new_points_equal_to_another_mesh_are_full() {
    let box_mesh = |name: &str, scale: f32, x: f32| {
        let s = 0.3 * scale;
        format!(
            "    def Mesh \"{name}\"\n    {{\n        uniform token subdivisionScheme = \"none\"\n        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]\n        int[] faceVertexIndices = [0, 1, 3, 2, 4, 6, 7, 5, 0, 4, 5, 1, 2, 3, 7, 6, 0, 2, 6, 4, 1, 5, 7, 3]\n        point3f[] points = [(-{s}, 0, -{s}), (-{s}, 0, {s}), (-{s}, {s}, -{s}), (-{s}, {s}, {s}), ({s}, 0, -{s}), ({s}, 0, {s}), ({s}, {s}, -{s}), ({s}, {s}, {s})]\n        double3 xformOp:translate = ({x}, 0, 1)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}"
        )
    };
    let before = format!("{}\n{}", box_mesh("A", 1.0, -0.5), box_mesh("B", 1.5, 0.5));
    let after = format!("{}\n{}", box_mesh("A", 1.5, -0.5), box_mesh("B", 1.5, 0.5));
    edit_applies_as("reshape_into_another", &before, &after, Applied::Full);
}

/// A transform above the render camera moves the camera: full.
#[test]
fn a_transform_above_the_camera_is_full() {
    edit_applies_as(
        "above_camera",
        "",
        "    double3 xformOp:translate = (0, 0.1, 0)\n    uniform token[] xformOpOrder = [\"xformOp:translate\"]",
        Applied::Full,
    );
}

fn rect_light(xf: &str, width: f32) -> String {
    format!(
        "    def RectLight \"Panel\"\n    {{\n        float inputs:width = {width}\n        float inputs:height = 0.6\n        float inputs:intensity = 8\n        double3 xformOp:translate = {xf}\n        float xformOp:rotateX = -90\n        uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateX\"]\n    }}"
    )
}

#[test]
fn moving_an_area_light_applies_as_geometry() {
    edit_applies_as(
        "move_area_light",
        &rect_light("(0, 3.5, 0)", 0.6),
        &rect_light("(0.6, 3.4, 0.3)", 0.6),
        Applied::Geometry,
    );
}

#[test]
fn resizing_an_area_light_applies_as_geometry() {
    edit_applies_as(
        "resize_area_light",
        &rect_light("(0, 3.5, 0)", 0.6),
        &rect_light("(0, 3.5, 0)", 1.4),
        Applied::Geometry,
    );
}

#[test]
fn turning_a_distant_light_applies_as_lights() {
    let sun = |angle: f32| {
        format!(
            "    def DistantLight \"Sun\"\n    {{\n        float inputs:intensity = 2\n        float xformOp:rotateX = {angle}\n        uniform token[] xformOpOrder = [\"xformOp:rotateX\"]\n    }}"
        )
    };
    edit_applies_as("turn_sun", &sun(-60.0), &sun(-20.0), Applied::Lights);
}

/// A 0.3-unit box mesh, `name`, its size times `scale`, at `x`.
fn box_mesh(name: &str, scale: f32, x: f32) -> String {
    let s = 0.3 * scale;
    format!(
        "    def Mesh \"{name}\"\n    {{\n        uniform token subdivisionScheme = \"none\"\n        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]\n        int[] faceVertexIndices = [0, 1, 3, 2, 4, 6, 7, 5, 0, 4, 5, 1, 2, 3, 7, 6, 0, 2, 6, 4, 1, 5, 7, 3]\n        point3f[] points = [(-{s}, 0, -{s}), (-{s}, 0, {s}), (-{s}, {s}, -{s}), (-{s}, {s}, {s}), ({s}, 0, -{s}), ({s}, 0, {s}), ({s}, {s}, -{s}), ({s}, {s}, {s})]\n        double3 xformOp:translate = ({x}, 0, 1)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}"
    )
}

/// Moving a prim inside a PointInstancer's prototypes moves every instance,
/// though the provenance records no geometry under it: full, not an empty
/// in-place update.
#[test]
fn moving_a_point_instancer_prototype_part_is_full() {
    let instancer = |x: f32| {
        format!(
            "    def PointInstancer \"Scatter\"\n    {{\n        rel prototypes = [<Protos/Box>]\n        int[] protoIndices = [0, 0, 0]\n        point3f[] positions = [(-1, 0, 0), (0, 0, 0.5), (1, 0, 0)]\n        def Scope \"Protos\"\n        {{\n            def Xform \"Box\"\n            {{\n{}\n            }}\n        }}\n    }}",
            box_mesh("Part", 0.6, x)
                .replace("\n    ", "\n            ")
                .replacen("    def", "            def", 1)
        )
    };
    edit_applies_as(
        "instancer_prototype",
        &instancer(0.0),
        &instancer(0.3),
        Applied::Full,
    );
}

/// A group another prim references: moving a mesh in it moves the copy the
/// reference composes too, which no geometry run says. Full.
#[test]
fn moving_a_mesh_another_prim_references_is_full() {
    let scene = |x: f32| {
        format!(
            "    def Xform \"Grp\"\n    {{\n{}\n    }}\n    def \"Copy\" (references = </scene/Grp>)\n    {{\n        double3 xformOp:translate = (0, 1.2, 0)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}",
            box_mesh("M", 1.0, x)
                .replace("\n    ", "\n        ")
                .replacen("    def", "        def", 1)
        )
    };
    edit_applies_as(
        "referenced_source",
        &scene(-0.5),
        &scene(0.2),
        Applied::Full,
    );
}

/// A reshape that makes a mesh identical to another's *reshaped* content is
/// full: the provenance keeps the keys in-place reshapes give.
#[test]
fn a_second_reshape_onto_a_reshaped_mesh_is_full() {
    let dir = work_dir("second_reshape");
    let graph = dir.join("graph.usda");
    let both = |a: f32, b: f32| {
        graph_layer(&format!(
            "{}\n{}",
            box_mesh("A", a, -0.5),
            box_mesh("B", b, 0.5)
        ))
    };
    write(&graph, &both(1.0, 1.5));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    write(&graph, &both(1.0, 2.0));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Geometry));
    converged(&session);
    // A now becomes what B became: a fresh import shares one mesh.
    write(&graph, &both(2.0, 2.0));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    assert!(converged(&session) == fresh(&root, &[graph]));
}

/// A change to a layer the session does not own, whose import then fails,
/// is imported by the next apply once the stage loads again, not forgotten.
#[test]
fn a_foreign_change_whose_import_failed_is_imported_later() {
    let dir = work_dir("failed_foreign");
    let (graph, other) = (dir.join("graph.usda"), dir.join("other.usda"));
    write(&graph, &graph_layer(""));
    write(&other, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda", "other.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    let first = converged(&session);
    let root_text = std::fs::read_to_string(&root).unwrap();
    write(&other, &graph_layer(&sky(3.0)));
    write(&root, "#usda 1.0\nthis does not parse {");
    assert!(session.apply().is_err(), "the root does not open");
    write(&root, &root_text);
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    let edited = converged(&session);
    assert_ne!(first, edited, "the foreign change shows");
    assert!(edited == fresh(&root, &[graph]));
}

/// A snapshot says the pixel aspect its image was framed with, so a host
/// writing its own EXR can record it: 1 for square pixels, and the derived
/// one under `adjustPixelAspectRatio` (the Cornell box's square aperture at
/// 24 × 18 needs pixels 0.75 wide), after an edit applied in place too.
#[test]
fn a_snapshot_names_its_pixel_aspect() {
    let dir = work_dir("snapshot_pixel_aspect");
    let root = write_root(&dir, &[]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(&root, &[]).expect("the layers import");
    converged(&session);
    assert_eq!(session.snapshot().expect("a render").pixel_aspect, 1.0);

    let text = std::fs::read_to_string(&root).expect("the root");
    let adjust = text.replace(
        "int crust:samplesPerPixel = 4",
        "int crust:samplesPerPixel = 4\n        uniform token aspectRatioConformPolicy = \
         \"adjustPixelAspectRatio\"",
    );
    assert_ne!(text, adjust);
    write(&root, &adjust);
    session.apply().expect("the edit applies");
    converged(&session);
    assert_eq!(session.snapshot().expect("a render").pixel_aspect, 0.75);
}

#[test]
fn a_snapshot_names_the_space_its_image_is_in() {
    use crust_core::color::Space;
    let dir = work_dir("snapshot_working_space");
    let root = write_root(&dir, &[]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(&root, &[]).expect("the layers import");
    converged(&session);
    let snapshot = session.snapshot().expect("a converged render");
    assert_eq!(snapshot.working_space, Space::LIN_REC709, "the default");

    let text = std::fs::read_to_string(&root).expect("the root");
    let acescg = text.replace(
        "int crust:samplesPerPixel = 4",
        "int crust:samplesPerPixel = 4\n        uniform token renderingColorSpace = \"acescg\"",
    );
    assert_ne!(text, acescg);
    write(&root, &acescg);
    session.apply().expect("the edit applies");
    converged(&session);
    let snapshot = session.snapshot().expect("a converged render");
    assert_eq!(
        Some(snapshot.working_space),
        Space::named("acescg"),
        "an authored renderingColorSpace"
    );
}

/// A stage for picks: a card bound to a material on the left, a
/// PointInstancer of boxes in the middle and a native instance on the right,
/// seen through `/World/Cam` with nothing behind them.
fn pick_stage() -> String {
    // A quad `x0..x1` × `-h..h` at depth 0, indented by `pad`, bound to the
    // red material when `bound`.
    let quad = |name: &str, x0: f32, x1: f32, h: f32, bound: bool, pad: &str| {
        let (schema, binding) = if bound {
            (
                " (prepend apiSchemas = [\"MaterialBindingAPI\"])",
                format!(
                    "
{pad}    rel material:binding = </World/Looks/Red>"
                ),
            )
        } else {
            ("", String::new())
        };
        format!(
            "{pad}def Mesh \"{name}\"{schema}
{pad}{{
{pad}    uniform token subdivisionScheme = \"none\"
{pad}    int[] faceVertexCounts = [4]
{pad}    int[] faceVertexIndices = [0, 1, 2, 3]
{pad}    point3f[] points = [({x0}, -{h}, 0), ({x1}, -{h}, 0), ({x1}, {h}, 0), ({x0}, {h}, 0)]{binding}
{pad}}}"
        )
    };
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 24)
        int crust:samplesPerPixel = 1
    }}
}}

class Xform "Proto"
{{
{proto}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 18
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def DistantLight "Sun"
    {{
        float inputs:intensity = 3
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
    }}

{card}

    def PointInstancer "Scatter"
    {{
        rel prototypes = [<Protos/Box>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, -0.45, 0), (0, 0.45, 0)]
        def Scope "Protos"
        {{
{boxes}
        }}
    }}

    def Xform "Inst" (
        instanceable = true
        references = </Proto>
    )
    {{
        double3 xformOp:translate = (2.2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
"#,
        proto = quad("Plate", -0.6, 0.6, 1.0, false, "    "),
        card = quad("Card", -3.2, -1.6, 1.0, true, "    "),
        boxes = quad("Box", -0.5, 0.5, 0.5, false, "            "),
    )
}

/// Every pixel's pick, row by row.
fn pick_all(session: &LiveSession, (width, height): (usize, usize)) -> Vec<Option<Pick>> {
    let mut picks = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            picks.push(session.pick(x, y).expect("a pick"));
        }
    }
    picks
}

#[test]
fn a_pick_names_the_prim_and_its_material_or_nothing() {
    let dir = work_dir("pick");
    let root = dir.join("root.usda");
    write(&root, &pick_stage());
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load(&root).expect("the stage imports");
    converged(&session);
    let snapshot = session.snapshot().expect("an image");
    let picks = pick_all(&session, (snapshot.width, snapshot.height));
    let named = |prim: &str| {
        picks
            .iter()
            .flatten()
            .filter(|p| p.prim.as_deref() == Some(prim))
            .collect::<Vec<_>>()
    };
    let card = named("/World/Card");
    assert!(!card.is_empty(), "the card is seen");
    assert!(
        card.iter()
            .all(|p| p.material.as_deref() == Some("/World/Looks/Red")),
        "with its bound material"
    );
    assert!(card.iter().all(|p| p.render == snapshot.render));
    assert!(
        !named("/World/Scatter").is_empty(),
        "a hit in a PointInstancer names the instancer"
    );
    assert!(
        !named("/World/Inst").is_empty(),
        "a hit in a native instance names the instance"
    );
    for p in picks.iter().flatten() {
        let prim = p.prim.as_deref().expect("every hit is named");
        assert!(
            ["/World/Card", "/World/Scatter", "/World/Inst"].contains(&prim),
            "no prototype's path: {prim}"
        );
    }
    // The corners see past everything.
    assert_eq!(session.pick(0, 0), Ok(None), "a miss");
    assert!(picks.iter().any(Option::is_none));
    let outside = session.pick(snapshot.width, 0);
    assert!(
        matches!(outside, Err(Error::InvalidArgument(_))),
        "{outside:?}"
    );
}

/// A pick is not answered from a scene that no longer holds the render's
/// geometry: after a failed load drops the scene, the image on screen is
/// the old render's, and the session says to ask again.
#[test]
fn a_pick_after_the_scene_was_dropped_is_busy() {
    let dir = work_dir("pick_busy");
    let root = dir.join("root.usda");
    write(&root, &pick_stage());
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load(&root).expect("the stage imports");
    converged(&session);
    assert!(session.pick(1, 12).is_ok());
    let broken = dir.join("broken.usda");
    write(&broken, "#usda 1.0\nthis does not parse {");
    assert!(
        session.load(&broken).is_err(),
        "the broken root does not open"
    );
    let busy = session.pick(1, 12);
    assert!(matches!(busy, Err(Error::Busy(_))), "{busy:?}");
    // Once a render starts again, picks are answered for it.
    session.load(&root).expect("the stage imports again");
    converged(&session);
    assert!(session.pick(1, 12).is_ok());
}

/// The shared refusal: a hit is named only against the import that drew it.
/// A second import of the same stage is other geometry, and a pick for the
/// first is refused.
#[test]
fn a_hit_is_named_only_against_the_geometry_rendered() {
    use super::scene::{Stale, import};
    let dir = work_dir("pick_stale");
    let root = dir.join("root.usda");
    write(&root, &pick_stage());
    let assets = || Arc::new(crust_assets::FileAssets::new()) as Arc<dyn crate::Assets>;
    let first = import(&root, None, None, assets()).expect("imports");
    let second = import(&root, None, None, assets()).expect("imports again");
    assert_ne!(first.geometry, second.geometry);
    let camera = *first.renderer.camera();
    let size = first.settings.get_dimensions();
    let centre = (size.0 / 2, size.1 / 2);
    let named = first
        .name_hit(first.geometry, &camera, size, centre)
        .expect("the scene drew it")
        .and_then(|n| n.prim);
    assert_eq!(named, Some("/World/Scatter"));
    assert!(matches!(
        second.name_hit(first.geometry, &camera, size, centre),
        Err(Stale)
    ));
}

/// The layer Mantle's IPR writes as the strongest sublayer: its settings as
/// a `def RenderSettings` on the settings prim the stage reads, and the
/// camera and resolution again as `over`s of the products the settings name,
/// whose own would win.
fn ipr_layer(camera: &str) -> String {
    format!(
        r#"#usda 1.0

over "Render"
{{
    over "beauty"
    {{
        uniform int2 resolution = (40, 24)
        rel camera = <{camera}>
    }}

    def RenderSettings "settings"
    {{
        uniform int2 resolution = (40, 24)
        rel camera = <{camera}>
        custom int crust:samplesPerPixel = 16
    }}
}}
"#
    )
}

/// A cooked stage as Mantle writes it: two cameras, a product the settings
/// name with its own camera, meshes under a sun and a dome.
const COOKED: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/cam1>
        rel products = </Render/beauty>
        int crust:maxDepth = 2
    }

    def RenderProduct "beauty"
    {
        rel camera = </World/cam1>
        token productName = "beauty.exr"
        rel orderedVars = </Render/C>
    }

    def RenderVar "C"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "color"
    }
}

def Xform "World"
{
    def Camera "cam1"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 0.6, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Camera "cam2"
    {
        float focalLength = 35
        double3 xformOp:translate = (1.8, 1.2, 5)
        float3 xformOp:rotateXYZ = (-8, 16, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }

    def DomeLight "Sky"
    {
        color3f inputs:color = (0.4, 0.5, 0.8)
    }

    def DistantLight "Sun"
    {
        float inputs:intensity = 3
    }

    def Mesh "Floor"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
    }

    def Mesh "Card"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 2, 0), (-1, 2, 0)]
    }
}
"#;

/// Mantle switches the IPR camera by rewriting `ipr.usda` with the new
/// camera's path on the settings and the products: a settings edit applied
/// in place, bitwise a fresh session's render at 16 spp, and so is switching
/// back.
#[test]
fn switching_the_ipr_camera_applies_in_place_as_a_fresh_import() {
    let dir = work_dir("ipr_camera_switch");
    let (ipr, cooked) = (dir.join("ipr.usda"), dir.join("cooked.usda"));
    write(&cooked, COOKED);
    write(&ipr, &ipr_layer("/World/cam1"));
    let root = dir.join("stage.usda");
    write(
        &root,
        "#usda 1.0\n(\n    subLayers = [@./ipr.usda@, @./cooked.usda@]\n)\n",
    );
    let owned = [ipr.clone()];
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, &owned)
        .expect("the layers import");
    let first = converged(&session);
    for camera in ["/World/cam2", "/World/cam1"] {
        write(&ipr, &ipr_layer(camera));
        session.apply().expect("the switch applies");
        assert_eq!(session.applied(), Some(Applied::Settings), "{camera}");
        let switched = converged(&session);
        assert!(
            switched == fresh(&root, &owned),
            "{camera}: as a fresh import"
        );
        assert_eq!(switched == first, camera == "/World/cam1", "{camera}");
    }
}

/// pCube2 with its translate sampled at 1 and 10: an animated cache.
fn animated_cube() -> String {
    graph_layer(&over_prim(
        "pCube2",
        "double3 xformOp:translate.timeSamples = {\n            1: (1.0612394919465933, 0.5, 0),\n            \
         10: (-0.6, 0.5, 0.4),\n        }",
    ))
}

/// A fresh session's image of the same files at `time`.
fn fresh_at(root: &Path, owned: &[PathBuf], time: Option<f64>) -> Vec<f32> {
    let session = LiveSession::new(Default::default()).expect("a session");
    session.set_time(time).expect("a finite time");
    session.load_owned(root, owned).expect("the layers import");
    converged(&session)
}

#[test]
fn a_time_change_renders_the_stage_at_the_new_time_as_a_fresh_import() {
    let dir = work_dir("time_change");
    let graph = dir.join("graph.usda");
    write(&graph, &animated_cube());
    let root = write_root(&dir, &["graph.usda"]);
    let owned = [graph.clone()];
    let session = LiveSession::new(Default::default()).expect("a session");
    session.set_time(Some(1.0)).expect("a finite time");
    session
        .load_owned(&root, &owned)
        .expect("the layers import");
    let at_1 = converged(&session);
    assert_eq!(session.snapshot().unwrap().time, Some(1.0));

    // The time alone moved: the next apply imports at the new time.
    session.set_time(Some(10.0)).expect("a finite time");
    assert_eq!(session.render(), 1, "set_time waits for the next apply");
    session.apply().expect("the time change applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    assert_eq!(session.render(), 2);
    let at_10 = converged(&session);
    assert_eq!(session.snapshot().unwrap().time, Some(10.0));
    assert_ne!(at_1, at_10, "the cube moved between the samples");
    assert!(
        at_10 == fresh_at(&root, &owned, Some(10.0)),
        "a time change renders as a fresh import at that time"
    );
    assert!(at_1 == fresh_at(&root, &owned, Some(1.0)));
}

#[test]
fn an_edit_at_a_time_still_applies_in_place() {
    let dir = work_dir("edit_at_time");
    let graph = dir.join("graph.usda");
    let lit = |i: f32| {
        animated_cube().replace(
            "over \"pCube2\"",
            &format!("over \"Sky\"\n    {{\n        float inputs:intensity = {i}\n    }}\n    over \"pCube2\""),
        )
    };
    write(&graph, &lit(1.0));
    let root = write_root(&dir, &["graph.usda"]);
    let owned = [graph.clone()];
    let session = LiveSession::new(Default::default()).expect("a session");
    session.set_time(Some(10.0)).expect("a finite time");
    session
        .load_owned(&root, &owned)
        .expect("the layers import");
    converged(&session);

    write(&graph, &lit(2.5));
    session.apply().expect("the edit applies");
    assert_eq!(session.applied(), Some(Applied::Lights));
    let edited = converged(&session);
    assert_eq!(session.snapshot().unwrap().time, Some(10.0));
    assert!(
        edited == fresh_at(&root, &owned, Some(10.0)),
        "in place at a time renders as a fresh import at that time"
    );
}

#[test]
fn a_time_that_is_not_finite_is_refused() {
    let session = LiveSession::new(Default::default()).expect("a session");
    for time in [f64::NAN, f64::INFINITY] {
        assert!(matches!(
            session.set_time(Some(time)),
            Err(Error::InvalidArgument(_))
        ));
    }
    session.set_time(None).expect("default values");
}

#[test]
fn without_a_time_the_snapshot_says_none() {
    let dir = work_dir("no_time");
    let root = write_root(&dir, &[]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load(&root).expect("the layer imports");
    converged(&session);
    assert_eq!(session.snapshot().unwrap().time, None);
}
