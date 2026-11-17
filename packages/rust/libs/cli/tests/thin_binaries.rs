//! Every binary crate is a thin wrapper over its entry library crate.
use std::fs;
use std::path::{Path, PathBuf};

/// The test sandbox receives `bins/` through `overrides.nix`; local runs read
/// it from the workspace.
fn bins() -> PathBuf {
    let dir = std::env::var_os("STOCKHOUSE_BINS_ROOT")
        .map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bins"), PathBuf::from);
    assert!(dir.is_dir(), "binary crates not found at {}", dir.display());
    dir
}

fn crates() -> Vec<PathBuf> {
    let mut crates: Vec<PathBuf> = fs::read_dir(bins()).unwrap().map(|e| e.unwrap().path()).collect();
    crates.sort();
    assert!(crates.iter().any(|c| c.ends_with("stockhouse")));
    crates
}

fn files(dir: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, root, out);
        } else {
            out.push(path.strip_prefix(root).unwrap().to_string_lossy().into_owned());
        }
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-BINARY-001"))]
#[test]
fn binaries_hold_only_manifest_and_main() {
    for krate in crates() {
        let mut found = Vec::new();
        files(&krate, &krate, &mut found);
        found.sort();
        assert_eq!(found, ["Cargo.toml", "src/main.rs"], "{}", krate.display());
        let manifest = fs::read_to_string(krate.join("Cargo.toml")).unwrap();
        for forbidden in ["[lib]", "[dev-dependencies]", "build =", "[[test]]", "[[bench]]"] {
            assert!(!manifest.contains(forbidden), "{}: {forbidden}", krate.display());
        }
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-BINARY-002"))]
#[test]
fn main_only_forwards_to_the_entry_crate() {
    for krate in crates() {
        let main = fs::read_to_string(krate.join("src/main.rs")).unwrap();
        let tokens: String = main.split_whitespace().collect();
        let name = krate.file_name().unwrap().to_string_lossy().replace('-', "_");
        let expected = format!("fnmain()->std::process::ExitCode{{{name}_cli::main(std::env::args().collect())}}");
        assert_eq!(tokens, expected, "{}", krate.display());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-BINARY-003"))]
#[test]
fn binaries_depend_only_on_their_entry_crate() {
    for krate in crates() {
        let manifest = fs::read_to_string(krate.join("Cargo.toml")).unwrap();
        let section = manifest.split("[dependencies]").nth(1).expect("dependencies section");
        let deps: Vec<&str> = section.lines().take_while(|l| !l.starts_with('[')).filter(|l| l.contains('=')).collect();
        let name = krate.file_name().unwrap().to_string_lossy();
        assert_eq!(deps, [format!("{name}-cli = {{ workspace = true }}")], "{}", krate.display());
    }
}
