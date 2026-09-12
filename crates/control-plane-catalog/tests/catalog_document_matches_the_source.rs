//! The published catalog document cannot drift from the source it is
//! generated from (issue #139). `examples/catalog-doc.rs` writes
//! `docs/control-plane/CATALOG.json`; this test re-derives what that
//! example would write and compares, and pins the hand-written JSON
//! Schema's module fields to the serialization actually emitted — so an
//! edit on either side fails here before a consumer ever sees it.

use std::path::Path;

fn docs_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/control-plane")
}

#[test]
fn the_published_catalog_document_matches_the_source() {
    let catalog = cratefield_catalog::curated();
    catalog
        .validate()
        .expect("the curated catalog must be coherent before it is published");
    let document = serde_json::json!({
        "schema": 1,
        "modules": catalog.modules,
    });
    let mut generated = serde_json::to_string_pretty(&document).expect("catalog serializes");
    generated.push('\n');

    let current = std::fs::read_to_string(docs_root().join("CATALOG.json"))
        .expect("docs/control-plane/CATALOG.json is checked in");
    assert_eq!(
        current, generated,
        "docs/control-plane/CATALOG.json is stale; regenerate with `cargo run -p \
         cratefield-catalog --example catalog-doc` and commit it"
    );
}

#[test]
fn the_schema_names_the_fields_the_serialization_actually_emits() {
    let catalog = cratefield_catalog::curated();
    let module = &catalog.modules[0];
    let module_json = serde_json::to_value(module).expect("a module serializes");
    let detail_json = serde_json::to_value(&module.detail).expect("a detail serializes");
    let release_json = serde_json::to_value(&module.releases[0]).expect("a release serializes");

    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(docs_root().join("catalog.schema.json"))
            .expect("docs/control-plane/catalog.schema.json is checked in"),
    )
    .expect("the schema is valid JSON");

    let module_props = &schema["$defs"]["module"]["properties"];
    for field in module_json.as_object().expect("object").keys() {
        assert!(
            module_props.get(field).is_some(),
            "catalog.schema.json has no `{field}` property for a module; \
             the schema and the serialization have drifted"
        );
    }
    assert_eq!(
        schema_required(&schema["$defs"]["module"]),
        sorted_keys(&module_json),
        "catalog.schema.json's module `required` list does not match the fields \
         the serialization emits"
    );

    let detail_props = &schema["$defs"]["detail"]["properties"];
    for field in detail_json.as_object().expect("object").keys() {
        assert!(
            detail_props.get(field).is_some(),
            "catalog.schema.json has no `{field}` property for a module detail; \
             the schema and the serialization have drifted"
        );
    }
    assert_eq!(
        schema_required(&schema["$defs"]["detail"]),
        sorted_keys(&detail_json),
        "catalog.schema.json's detail `required` list does not match the fields \
         the serialization emits"
    );

    let release_props = &schema["$defs"]["release"]["properties"];
    for field in release_json.as_object().expect("object").keys() {
        assert!(
            release_props.get(field).is_some(),
            "catalog.schema.json has no `{field}` property for a release; \
             the schema and the serialization have drifted"
        );
    }

    // The document itself parses back into the catalog it came from,
    // under the envelope version consumers branch on.
    let raw = std::fs::read_to_string(docs_root().join("CATALOG.json"))
        .expect("CATALOG.json is checked in");
    let document: serde_json::Value = serde_json::from_str(&raw).expect("CATALOG.json is JSON");
    assert_eq!(
        document["schema"], 1,
        "the envelope schema version moved; consumers branch on it, so \
         examples/catalog-doc.rs and this test must move with it"
    );
    let parsed: cratefield_catalog::Catalog =
        serde_json::from_str(&raw).expect("the published document deserializes as a Catalog");
    assert_eq!(
        document["schema"], 1,
        "the envelope schema version moved; consumers branch on it, so \
         examples/catalog-doc.rs and this test must move with it"
    );
    assert_eq!(
        parsed.modules.len(),
        catalog.modules.len(),
        "the published document carries a different module set than curated()"
    );
}

fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("a serialized struct is a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn schema_required(def: &serde_json::Value) -> Vec<String> {
    def["required"]
        .as_array()
        .expect("the schema def carries a required list")
        .iter()
        .map(|v| v.as_str().expect("required entries are strings").to_owned())
        .collect()
}
