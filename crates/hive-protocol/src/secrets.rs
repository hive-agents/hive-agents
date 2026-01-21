use crate::errors::ProtocolError;
use crate::types::SecretRef;

impl SecretRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        let value = self.0.as_str();
        if value.is_empty() {
            return Err(ProtocolError::MissingField("secret_ref"));
        }

        let (scheme, rest) = match value.split_once(':') {
            Some(parts) => parts,
            None => {
                return Err(ProtocolError::InvalidField {
                    field: "secret_ref",
                    details: "missing scheme prefix".to_string(),
                })
            }
        };

        if scheme.is_empty() {
            return Err(ProtocolError::InvalidField {
                field: "secret_ref",
                details: "empty scheme".to_string(),
            });
        }

        if !scheme
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(ProtocolError::InvalidField {
                field: "secret_ref",
                details: "scheme must be lowercase ascii, digits, '-' or '_'".to_string(),
            });
        }

        if rest.is_empty() {
            return Err(ProtocolError::InvalidField {
                field: "secret_ref",
                details: "missing reference body".to_string(),
            });
        }

        if rest.trim().is_empty() {
            return Err(ProtocolError::InvalidField {
                field: "secret_ref",
                details: "reference body is blank".to_string(),
            });
        }

        Ok(())
    }
}
