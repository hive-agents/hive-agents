use std::path::PathBuf;

use schemars::schema_for;

mod schema_types {
    #![allow(dead_code)]
    #![allow(clippy::all)]
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/types.rs"));
}

fn main() {
    println!("cargo:rerun-if-changed=src/types.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let profile_schema = schema_for!(schema_types::Profile);
    let local_settings_schema = schema_for!(schema_types::LocalSettings);
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile_schema_path = manifest_dir.join("../../docs/specs/profile.schema.json");
    let local_settings_schema_path =
        manifest_dir.join("../../docs/specs/local_settings.schema.json");

    if let Some(parent) = profile_schema_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            panic!("failed to create schema dir {parent:?}: {err}");
        }
    }

    write_schema(&profile_schema_path, &profile_schema, "profile");
    write_schema(&local_settings_schema_path, &local_settings_schema, "local_settings");
}

fn write_schema(path: &PathBuf, schema: &schemars::schema::RootSchema, label: &str) {
    let json = serde_json::to_string_pretty(schema)
        .unwrap_or_else(|err| panic!("failed to serialize {label} schema: {err}"));
    std::fs::write(path, json)
        .unwrap_or_else(|err| panic!("failed to write {path:?}: {err}"));
}
