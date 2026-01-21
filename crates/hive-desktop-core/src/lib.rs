mod errors;
mod filesystem;
mod health;
mod mount;
mod pairing;
mod supervisor;
mod tunnel;

use std::path::PathBuf;

use hive_protocol::SecretRef;

pub use errors::{DesktopError, Result};
pub use pairing::{default_device_name, generate_device_keypair, pair_envelope, PairingOutcome};
pub use supervisor::{
    LogLevel, LogLine, LogSource, Status, SupervisorConfig, SupervisorHandle, SupervisorState,
};

pub trait SecretStore: Send + Sync {
    fn resolve_text(&self, secret: &SecretRef) -> Result<String>;
    fn resolve_path(&self, secret: &SecretRef) -> Result<PathBuf>;
}
