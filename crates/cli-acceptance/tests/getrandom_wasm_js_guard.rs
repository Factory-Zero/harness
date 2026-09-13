//! getrandom always resolves with its `wasm_js` backend on wasm (issue #144).
//!
//! On `wasm32-unknown-unknown`, getrandom 0.4 panics at runtime unless the
//! `wasm_js` backend is opted into and `default-features` is off — the panic
//! surfaces as `unreachable executed` inside a live Worker, which `cargo
//! test`, `worker-build` and even `wrangler dev` all pass right up until the
//! first request needs a random byte. The workspace pins the right shape in
//! `[workspace.dependencies]`, and every crate that needs its own declaration
//! pins it too (module-linkedin splits it by target because the plain backend
//! pulls wasm-bindgen onto the host), but nothing checked it.
//!
//! Same shape as `wasm_dispatcher_guard.rs`: the rule is a test, the test
//! proves the detector fires, and the scan is line-based over `Cargo.toml`
//! text because the feature choice exists nowhere else — `Cargo.lock` does
//! not record features.

mod common;

use std::path::Path;

use common::{dependency_entry, enclosing_section, relative_to, repo_root, workspace_manifests};

/// Every `getrandom =` declaration in a manifest, as (offset, entry text).
fn getrandom_declarations(source: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find("getrandom") {
        let offset = from + at;
        let rest = &source[offset..];
        let is_key = rest.starts_with("getrandom")
            && rest["getrandom".len()..].trim_start().starts_with('=');
        if is_key {
            found.push((offset, dependency_entry(source, offset)));
        }
        from = offset + "getrandom".len();
    }
    found
}

/// Whether the section above a declaration keeps it off the wasm build —
/// the shape module-linkedin uses to give the host a plain getrandom.
fn gated_off_wasm(source: &str, offset: usize) -> bool {
    enclosing_section(source, offset).is_some_and(|section| {
        section.contains("target.") && section.contains("not(") && section.contains("wasm")
    })
}

/// Whether one declaration carries the wasm backend: either inherited from
/// the workspace pin or spelled out in full.
fn declares_wasm_js(entry: &str) -> bool {
    entry.contains("workspace = true")
        || (entry.contains("default-features = false") && entry.contains("wasm_js"))
}

fn violations_in(relative: &str, source: &str, is_root: bool) -> Vec<String> {
    let mut violations = Vec::new();
    for (offset, entry) in getrandom_declarations(source) {
        if is_root {
            // The workspace pin is the shape every `workspace = true`
            // inherits; it must be correct in full, not by inheritance.
            if !declares_wasm_js(entry) {
                violations.push(format!(
                    "{relative}: the workspace getrandom pin must be \
                     `default-features = false` with the `wasm_js` feature"
                ));
            }
        } else if !declares_wasm_js(entry) && !gated_off_wasm(source, offset) {
            violations.push(format!(
                "{relative}: a getrandom declaration without `wasm_js` reaches \
                 wasm builds and panics on first use — take it via `workspace = \
                 true`, or split it by target as module-linkedin does"
            ));
        }
    }
    violations
}

#[test]
fn every_getrandom_declaration_carries_the_wasm_js_backend() {
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

    assert!(
        violations.is_empty(),
        "getrandom without its `wasm_js` backend panics on first use inside a \
         workerd isolate (issue #144):\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn the_guard_fires_on_a_bare_version_pin() {
    let source = "[dependencies]\ngetrandom = \"0.4\"\n";
    assert_eq!(violations_in("crates/x/Cargo.toml", source, false).len(), 1);
}

#[test]
fn the_guard_fires_on_default_features_left_on() {
    let source = "[dependencies]\ngetrandom = { version = \"0.4\", features = [\"wasm_js\"] }\n";
    assert_eq!(violations_in("crates/x/Cargo.toml", source, false).len(), 1);
}

#[test]
fn the_guard_accepts_the_workspace_pin() {
    let source = "[dependencies]\ngetrandom = { workspace = true }\n";
    assert!(violations_in("crates/x/Cargo.toml", source, false).is_empty());
}

#[test]
fn the_guard_accepts_a_target_split_declaration() {
    // module-linkedin's shape: plain on the host, wasm_js on wasm.
    let source = concat!(
        "[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]\n",
        "getrandom = \"0.4\"\n",
        "\n",
        "[target.'cfg(target_family = \"wasm\")'.dependencies]\n",
        "getrandom = { version = \"0.4\", default-features = false, features = [\"wasm_js\"] }\n",
    );
    assert!(violations_in("crates/module-linkedin/Cargo.toml", source, false).is_empty());
}

#[test]
fn the_guard_holds_the_workspace_pin_to_the_full_shape() {
    let source = "[workspace.dependencies]\ngetrandom = \"0.4\"\n";
    assert_eq!(violations_in("Cargo.toml", source, true).len(), 1);
}

#[test]
fn the_entry_reader_sees_the_whole_multi_line_pin() {
    // The root manifest formats the features across lines; a reader that
    // stopped at the first line would miss `wasm_js` and fire on a correct
    // pin.
    let source = "[workspace.dependencies]\n\
                  getrandom = { version = \"0.4.3\", default-features = false, features = [\n\
                  \"wasm_js\",\n\
                  ] }\n\n\
                  [dependencies]\n";
    let (_, entry) = &getrandom_declarations(source)[0];
    assert!(entry.contains("wasm_js"));
    assert!(!entry.contains("[dependencies]"));
}

#[test]
fn the_guard_sees_the_real_workspace_and_linkedin_manifests() {
    let root = repo_root();
    let root_manifest = std::fs::read_to_string(Path::new(&root).join("Cargo.toml")).unwrap();
    assert!(violations_in("Cargo.toml", &root_manifest, true).is_empty());
    let linkedin =
        std::fs::read_to_string(Path::new(&root).join("crates/module-linkedin/Cargo.toml"))
            .unwrap();
    assert!(violations_in("crates/module-linkedin/Cargo.toml", &linkedin, false).is_empty());
}
