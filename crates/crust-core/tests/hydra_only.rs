//! The delegate reads Hydra only (design D1): no file under
//! `src/scene/hydra/` (the delegate's scene side) or
//! `crust-render/src/delegate/` (its render control, when built) names a
//! stage, Moho's stage scene index or Moho's generic USD data. What only the
//! host's stage can say arrives as `HostFacts`, which the host answers.
//!
//! Test-only items (an item under `#[cfg(test)]`, a test module) and comment
//! lines are skipped: a unit test builds its scene index over a stage of its
//! own, through the host's helpers.

use std::path::{Path, PathBuf};

/// What the delegate may not name.
const FORBIDDEN: &[&str] = &[
    "moho::stage",
    "openusd::usd",
    "StageSceneIndex",
    "tokens::usd::",
    "tokens::usd as",
    "tokens::usd;",
    "tokens::usd}",
    "tokens::usd,",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `src` without its test-only items and comment lines: an item after a
/// `#[cfg(test)]` or `#[cfg(all(test, …))]` attribute is skipped up to the
/// brace that closes it (literals in tests hold balanced braces).
fn delegate_code(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut skipping = false;
    let mut depth = 0i64;
    let mut opened = false;
    for (n, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if !skipping && (t.starts_with("#[cfg(test)]") || t.starts_with("#[cfg(all(test")) {
            skipping = true;
            depth = 0;
            opened = false;
            continue;
        }
        if skipping {
            for c in line.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && depth <= 0 {
                skipping = false;
            }
            continue;
        }
        if t.starts_with("//") {
            continue;
        }
        out.push((n + 1, line));
    }
    out
}

#[test]
fn the_delegate_names_no_stage() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src/scene/hydra"), &mut files);
    rust_files(&root.join("../crust-render/src/delegate"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("hydra/mod.rs")),
        "src/scene/hydra/ is where the delegate lives"
    );
    let mut offenders = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).expect("a readable source");
        for (n, line) in delegate_code(&src) {
            if let Some(name) = FORBIDDEN.iter().find(|f| line.contains(**f)) {
                offenders.push(format!(
                    "{}:{n}: names {name}: {}",
                    file.display(),
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the delegate reads Hydra only; ask the host through HostFacts:\n{}",
        offenders.join("\n")
    );
}
