use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unsupported profile version {0}")]
    UnsupportedProfileVersion(u32),
    #[error("missing required field {0}")]
    MissingField(&'static str),
    #[error("invalid field {field}: {details}")]
    InvalidField {
        field: &'static str,
        details: String,
    },
    #[error("duplicate tunnel name {0}")]
    DuplicateTunnelName(String),
}
