use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub const PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct ProfileId(pub Uuid);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct SecretRef(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Profile {
    pub profile_version: u32,
    pub profile_id: ProfileId,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub ssh: SshConfig,
    pub tunnels: Vec<TunnelConfig>,
    pub juicefs: JuiceFsConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub host_key_fingerprint_sha256: String,
    pub identity_key_ref: SecretRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TunnelConfig {
    pub name: String,
    pub remote_host: String,
    pub remote_port: u16,
    pub local_bind_host: String,
    pub local_port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JuiceFsConfig {
    pub volume_name: String,
    pub meta: JuiceFsMetaConfig,
    pub object: JuiceFsObjectConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JuiceFsMetaConfig {
    pub engine: MetaEngine,
    pub user: String,
    pub database: String,
    pub password_ref: SecretRef,
    pub dsn_template: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JuiceFsObjectConfig {
    pub storage: StorageKind,
    pub bucket_name: String,
    pub endpoint_mode: EndpointMode,
    pub bucket_url_template: String,
    pub access_key_ref: SecretRef,
    pub secret_key_ref: SecretRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MetaEngine {
    Postgres,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StorageKind {
    S3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EndpointMode {
    Tunneled,
    Direct,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalSettings {
    pub profile_id: ProfileId,
    pub mountpoint: Mountpoint,
    pub cache: CacheSettings,
    pub runtime: RuntimeSettings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mountpoint {
    pub platform: Platform,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CacheSettings {
    pub cache_dir: PathBuf,
    pub cache_size_mib: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RuntimeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_local_postgres_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_local_s3_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_at: Option<DateTime<Utc>>,
}
