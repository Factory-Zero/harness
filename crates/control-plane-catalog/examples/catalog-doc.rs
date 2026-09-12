//! Generates `docs/control-plane/CATALOG.json` (issue #139): the curated
//! catalog as a static, versioned, machine-readable document a user or
//! an agent reads without compiling Rust. Run without arguments to write
//! the file, or with `--check` to verify the checked-in copy has no
//! drift (CI fails the run on drift).
//!
//! ```text
//! cargo run -p cratefield-catalog --example catalog-doc          # write
//! cargo run -p cratefield-catalog --example catalog-doc -- --check
//! ```
//!
//! Host-only tooling, the same shape as `errors-doc`,
//! `compatibility-doc` and `pricing-doc`: the library never touches
//! `std::fs`, and this only keeps the checked-in document saying what
//! `curated()` says. The JSON Schema that documents the artifact's
//! shape lives beside it at `docs/control-plane/catalog.schema.json`
//! and is hand-maintained; the crate's tests pin the artifact to the
//! source and the schema's module fields to the serialization.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/control-plane/CATALOG.json");
    let check = std::env::args().any(|arg| arg == "--check");

    let catalog = cratefield_catalog::curated();
    catalog
        .validate()
        .expect("the curated catalog must be coherent before it is published");

    // `schema` is the artifact's own version, not the harness API: it
    // bumps when the envelope's shape changes, so a consumer can branch
    // without parsing module entries. Modules stay in the order the
    // wizard shows them — the order is part of the document.
    let document = serde_json::json!({
        "schema": 1,
        "modules": catalog.modules,
    });
    let mut generated = serde_json::to_string_pretty(&document).expect("catalog serializes");
    generated.push('\n');

    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != generated {
            eprintln!(
                "docs/control-plane/CATALOG.json is stale; regenerate with `cargo run -p \
                 cratefield-catalog --example catalog-doc` and commit it"
            );
            std::process::exit(1);
        }
        println!(
            "docs/control-plane/CATALOG.json is up to date ({} modules)",
            catalog.modules.len()
        );
        return;
    }

    std::fs::write(&path, generated).expect("write docs/control-plane/CATALOG.json");
    println!(
        "wrote {} ({} modules)",
        path.display(),
        catalog.modules.len()
    );
}
