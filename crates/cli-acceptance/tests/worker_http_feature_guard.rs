//! The workers-rs `http` feature stays off, everywhere (issue #144).
//!
//! With `worker`'s `http` feature enabled, D1 writes hang under
//! workerd/miniflare (worker 0.8.5, verified empirically and written down in
//! the workspace `Cargo.toml` note). Cargo unifies features across the whole
//! build graph, so one crate anywhere declaring `features = ["http"]` turns
//! it on for every Worker in the repository — `cargo test`, `worker-build`
//! and the rest of the suite keep passing while every request hangs.
//!
//! Same shape as `wasm_dispatcher_guard.rs` and `getrandom_wasm_js_guard.rs`:
//! the rule is a test, the test proves the detector fires, and the scan is
//! over `Cargo.toml` text because features are recorded nowhere else —
//! `Cargo.lock` does not carry them.

mod common;

use common::{dependency_entry, relative_to, repo_root, workspace_manifests};

/// Every `worker =` dependency declaration in a manifest. The exact key —
/// `worker-macros`, `worker-build`, `kv-workspace` and friends do not match,
/// because the hang lives in the `worker` crate's own feature.
fn worker_declarations(source: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find("worker") {
        let offset = from + at;
        let rest = &source[offset..];
        let is_key = rest.starts_with("worker")
            && rest["worker".len()..].trim_start().starts_with('=')
            && !source[..offset]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if is_key {
            found.push((offset, dependency_entry(source, offset)));
        }
        from = offset + "worker".len();
    }
    found
}

fn violations_in(relative: &str, source: &str, is_root: bool) -> Vec<String> {
    let mut violations = Vec::new();
    for (_, entry) in worker_declarations(source) {
        if entry.contains("\"http\"") {
            violations.push(format!(
                "{relative}: the workers-rs `http` feature hangs D1 writes under \
                 workerd (issue #103) — keep it off"
            ));
        }
        if entry.contains("default-features = true") {
            violations.push(format!(
                "{relative}: worker's default features pull in `http` — declare \
                 `default-features = false` with the features you need"
            ));
        }
        if is_root && !entry.contains("default-features = false") {
            violations.push(format!(
                "{relative}: the workspace worker pin must carry \
                 `default-features = false`, it is the shape every \
                 `workspace = true` inherits"
            ));
        }
    }
    violations
}

fn scan_workspace() -> Vec<String> {
    let root = repo_root();
    let mut manifests = Vec::new();
    workspace_manifests(&root, &mut manifests);
    assert!(
        manifests.len() > 30,
        "the walk found only {} manifests, so it is not walking the workspace",
        manifests.len()
    );
    let mut violations = Vec::new();
    for path in manifests {
        let relative = relative_to(&root, &path);
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        violations.extend(violations_in(&relative, &source, relative == "Cargo.toml"));
    }
    violations
}

#[test]
fn no_manifest_enables_the_worker_http_feature() {
    let violations = scan_workspace();
    assert!(
        violations.is_empty(),
        "worker's `http` feature hangs D1 writes under workerd, and Cargo \
         unifies it across the whole graph — one declaration is enough to \
         hang every Worker (issue #144):\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn the_guard_fires_on_the_http_feature() {
    let source = "[dependencies]\nworker = { version = \"0.8\", features = [\"http\"] }\n";
    assert_eq!(violations_in("crates/x/Cargo.toml", source, false).len(), 1);
}

#[test]
fn the_guard_fires_on_default_features() {
    let source = "[dependencies]\nworker = { version = \"0.8\", default-features = true }\n";
    assert_eq!(violations_in("crates/x/Cargo.toml", source, false).len(), 1);
}

#[test]
fn the_guard_accepts_the_workspace_shape() {
    let source = "[dependencies]\nworker = { workspace = true }\n";
    assert!(violations_in("crates/x/Cargo.toml", source, false).is_empty());
}

#[test]
fn the_guard_holds_the_workspace_pin_to_explicit_defaults() {
    let source =
        "[workspace.dependencies]\nworker = { version = \"0.8.5\", features = [\"d1\"] }\n";
    assert_eq!(violations_in("Cargo.toml", source, true).len(), 1);
}

#[test]
fn the_guard_does_not_fire_on_worker_named_dependencies() {
    // `worker-macros` is a different crate with an http feature of its own
    // that does not hang anything; the exact key is what matters.
    let source = "[dependencies]\nworker-macros = { version = \"0.8\", features = [\"http\"] }\n";
    assert!(violations_in("crates/x/Cargo.toml", source, false).is_empty());
}

#[test]
fn the_guard_reads_the_real_root_manifest_clean() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(violations_in("Cargo.toml", &source, true).is_empty());
}
