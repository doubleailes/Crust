//! Import parity on the committed samples (the `moho-import` change, spec
//! `import-parity`): each sample's `crust check --json` report, timings
//! excluded, against the baseline recorded from the direct importer.
//!
//! The `crust` process this runs is built with crust-core's `parity` feature
//! (the dev-dependency in `Cargo.toml`), so each check also compares the
//! imported scene's digest with its baseline (`crust_core::parity`), and with
//! `CRUST_PARITY_IMAGES=1` its 16 spp image. `CRUST_RECORD_BASELINES=1`
//! records every baseline instead.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace's `samples/`.
fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .canonicalize()
        .expect("samples/")
}

/// Samples that reference files outside the repository, with the file each
/// needs: compared only where that file exists, and listed as skipped
/// otherwise (in CI). They are also the heavy ones (four to ten minutes each
/// unoptimised), so an unoptimised build compares them only with
/// `CRUST_PARITY_EXTERNAL=1`; an optimised one (`cargo test --release`, the
/// image run) always does.
const EXTERNAL: &[(&str, &str)] = &[
    ("materialx_lion.usda", "MaterialXTeapotLion-1.0"),
    ("materialx_teapot.usda", "MaterialXTeapotLion-1.0"),
    (
        "materialx_showcase.usda",
        "/home/philippe.llerena/Workspace/samples/MaterialXTeapotLion-1.0",
    ),
];

/// Every committed top-level `samples/*.usd*` stage.
fn sample_stages() -> Vec<PathBuf> {
    let mut stages: Vec<PathBuf> = std::fs::read_dir(samples())
        .expect("list samples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "usd" | "usda" | "usdc" | "usdz"))
        })
        .collect();
    stages.sort();
    stages
}

/// Where the report baselines live, beside crust-core's digests.
fn baseline_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../crust-core/tests/data/import_baselines/check")
}

/// `crust check --json -` on `stage`, with the import's phases (timings and
/// memory samples) removed and the workspace written as `$ROOT`.
fn report(stage: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(samples())
        .args(["check", "-i"])
        .arg(stage)
        .args(["--json", "-", "-l", "error"])
        .output()
        .expect("run crust check");
    assert!(
        out.status.success(),
        "crust check {} failed:\n{}",
        stage.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "crust check {} printed no report ({e}):\n{}",
            stage.display(),
            String::from_utf8_lossy(&out.stdout)
        )
    });
    if let Some(report) = json.as_object_mut() {
        report.remove("import");
    }
    let text = serde_json::to_string_pretty(&json).expect("re-serialize");
    let root = samples()
        .parent()
        .expect("the workspace")
        .display()
        .to_string();
    text.replace(&root, "$ROOT") + "\n"
}

/// The report baseline comparison for one sample: `None` when equal (or
/// recorded), else what differs.
fn compare(stage: &Path, record: bool) -> Option<String> {
    let name = stage
        .file_name()
        .expect("a file")
        .to_string_lossy()
        .into_owned();
    let dir = baseline_dir();
    let actual = report(stage);
    let file = dir.join(format!("{name}.json"));
    if record {
        std::fs::create_dir_all(&dir).expect("baseline dir");
        std::fs::write(&file, &actual).expect("record the baseline");
        return None;
    }
    let Ok(expected) = std::fs::read_to_string(&file) else {
        return Some(format!(
            "{name}: no baseline {} — record it with CRUST_RECORD_BASELINES=1",
            file.display()
        ));
    };
    if expected == actual {
        return None;
    }
    let line = expected
        .lines()
        .zip(actual.lines())
        .position(|(a, b)| a != b)
        .unwrap_or(expected.lines().count().min(actual.lines().count()));
    Some(format!(
        "{name}: the check report differs from {} at line {}:\n  baseline: {}\n  import:   {}",
        file.display(),
        line + 1,
        expected.lines().nth(line).unwrap_or("<end>"),
        actual.lines().nth(line).unwrap_or("<end>")
    ))
}

#[test]
fn every_sample_imports_as_its_baseline() {
    let record = std::env::var("CRUST_RECORD_BASELINES").is_ok_and(|v| v == "1");
    let external = cfg!(not(debug_assertions))
        || std::env::var("CRUST_PARITY_EXTERNAL").is_ok_and(|v| v == "1");
    let mut stages = Vec::new();
    for stage in sample_stages() {
        let name = stage
            .file_name()
            .expect("a file")
            .to_string_lossy()
            .into_owned();
        if let Some((_, needs)) = EXTERNAL.iter().find(|(s, _)| *s == name) {
            let needed = samples().join(needs);
            if !needed.exists() {
                eprintln!("skipped {name}: needs {} (not committed)", needed.display());
                continue;
            }
            if !external {
                eprintln!("skipped {name}: external and heavy, set CRUST_PARITY_EXTERNAL=1");
                continue;
            }
        }
        stages.push(stage);
    }
    // Each check is its own process: run them side by side.
    let failures: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = stages
            .iter()
            .map(|stage| scope.spawn(move || compare(stage, record)))
            .collect();
        runs.into_iter()
            .filter_map(|run| run.join().expect("a sample's check panicked"))
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(stages.len() >= 40, "only {} samples compared", stages.len());
}
