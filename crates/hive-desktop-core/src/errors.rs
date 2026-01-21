use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesktopError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
    #[error("ssh error: {0}")]
    Ssh(String),
    #[error("mount error: {0}")]
    Mount(String),
    #[error("preflight error: {0}")]
    Preflight(String),
    #[error("secret not found: {0}")]
    SecretNotFound(String),
    #[error("invalid profile: {0}")]
    InvalidProfile(String),
    #[error("supervisor error: {0}")]
    Supervisor(String),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(&'static str),
    #[error("timeout while {0}")]
    Timeout(&'static str),
}

pub type Result<T> = std::result::Result<T, DesktopError>;
