use crate::types::{LocalSettings, Profile};
use schemars::{schema::RootSchema, schema_for};
use std::path::Path;

pub fn profile_schema() -> RootSchema {
    schema_for!(Profile)
}

pub fn local_settings_schema() -> RootSchema {
    schema_for!(LocalSettings)
}

pub fn write_profile_schema(path: impl AsRef<Path>) -> std::io::Result<()> {
    let schema = profile_schema();
    let json = serde_json::to_string_pretty(&schema)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
    std::fs::write(path, json)
}

pub fn write_local_settings_schema(path: impl AsRef<Path>) -> std::io::Result<()> {
    let schema = local_settings_schema();
    let json = serde_json::to_string_pretty(&schema)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
    std::fs::write(path, json)
}
