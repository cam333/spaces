use std::{env, fs, path::PathBuf};

/// Merge the protocol schema's twelve self-contained documents into one
/// draft-07 document and generate Rust types from it. The schema is the
/// committed artifact the Node runtime generates from its zod definitions,
/// so this keeps the client's types tied to the server's contract.
fn main() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    let schema_path = repo_root
        .join("docs")
        .join("protocol")
        .join("spaces-proto.schema.json");
    println!("cargo:rerun-if-changed={}", schema_path.display());

    let raw = fs::read_to_string(&schema_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", schema_path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("schema is not valid json: {e}"));

    let mut definitions = serde_json::Map::new();
    let entries = doc
        .get("schemas")
        .and_then(|s| s.as_object())
        .unwrap_or_else(|| panic!("schema document has no `schemas` object"));
    for entry in entries.values() {
        if let Some(defs) = entry.get("definitions").and_then(|d| d.as_object()) {
            for (name, body) in defs {
                if let Some(prev) = definitions.insert(name.clone(), body.clone()) {
                    assert_eq!(&prev, body, "conflicting definitions for `{name}`");
                }
            }
        }
    }

    let merged = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "SpacesProto",
        "type": "object",
        "definitions": definitions,
    });

    let schema: schemars::schema::RootSchema =
        serde_json::from_value(merged).unwrap_or_else(|e| panic!("merged schema rejected: {e}"));
    let mut settings = typify::TypeSpaceSettings::default();
    settings.with_derive("PartialEq".to_string());
    let mut type_space = typify::TypeSpace::new(&settings);
    type_space
        .add_root_schema(schema)
        .unwrap_or_else(|e| panic!("typify failed: {e}"));

    let contents = prettyplease::unparse(
        &syn::parse2::<syn::File>(type_space.to_stream()).expect("generated code did not parse"),
    );
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR unset")).join("proto_types.rs");
    fs::write(&out, contents).unwrap_or_else(|e| panic!("cannot write {}: {e}", out.display()));
}
