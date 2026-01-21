use crate::errors::ProtocolError;
use crate::types::{
    CacheSettings, EndpointMode, JuiceFsConfig, JuiceFsMetaConfig, JuiceFsObjectConfig,
    LocalSettings, Mountpoint, Profile, RuntimeSettings, SshConfig, TunnelConfig, PROFILE_VERSION,
};
use std::collections::HashSet;

impl Profile {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.profile_version != PROFILE_VERSION {
            return Err(ProtocolError::UnsupportedProfileVersion(
                self.profile_version,
            ));
        }

        ensure_non_empty("display_name", &self.display_name)?;
        self.ssh.validate()?;

        if self.tunnels.is_empty() {
            return Err(ProtocolError::MissingField("tunnels"));
        }

        let mut seen = HashSet::new();
        for tunnel in &self.tunnels {
            tunnel.validate()?;
            if !seen.insert(tunnel.name.as_str()) {
                return Err(ProtocolError::DuplicateTunnelName(tunnel.name.clone()));
            }
        }

        self.juicefs.validate()?;
        Ok(())
    }
}

impl SshConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        ensure_non_empty("ssh.host", &self.host)?;
        ensure_non_empty("ssh.user", &self.user)?;
        ensure_non_empty(
            "ssh.host_key_fingerprint_sha256",
            &self.host_key_fingerprint_sha256,
        )?;

        if !self.host_key_fingerprint_sha256.starts_with("SHA256:") {
            return Err(ProtocolError::InvalidField {
                field: "ssh.host_key_fingerprint_sha256",
                details: "expected SHA256: prefix".to_string(),
            });
        }

        self.identity_key_ref.validate()?;
        Ok(())
    }
}

impl TunnelConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        ensure_non_empty("tunnel.name", &self.name)?;
        ensure_non_empty("tunnel.remote_host", &self.remote_host)?;
        ensure_non_empty("tunnel.local_bind_host", &self.local_bind_host)?;

        if self.remote_port == 0 {
            return Err(ProtocolError::InvalidField {
                field: "tunnel.remote_port",
                details: "must be non-zero".to_string(),
            });
        }

        Ok(())
    }
}

impl JuiceFsConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        ensure_non_empty("juicefs.volume_name", &self.volume_name)?;
        self.meta.validate()?;
        self.object.validate()?;
        Ok(())
    }
}

impl JuiceFsMetaConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        ensure_non_empty("juicefs.meta.user", &self.user)?;
        ensure_non_empty("juicefs.meta.database", &self.database)?;
        ensure_non_empty("juicefs.meta.dsn_template", &self.dsn_template)?;
        ensure_template_contains(
            "juicefs.meta.dsn_template",
            &self.dsn_template,
            "{local_postgres_port}",
        )?;
        self.password_ref.validate()?;
        Ok(())
    }
}

impl JuiceFsObjectConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        ensure_non_empty("juicefs.object.bucket_name", &self.bucket_name)?;
        ensure_non_empty(
            "juicefs.object.bucket_url_template",
            &self.bucket_url_template,
        )?;

        if matches!(self.endpoint_mode, EndpointMode::Tunneled)
            && !self.bucket_url_template.contains("{local_s3_port}")
        {
            return Err(ProtocolError::InvalidField {
                field: "juicefs.object.bucket_url_template",
                details: "expected {local_s3_port} placeholder".to_string(),
            });
        }

        self.access_key_ref.validate()?;
        self.secret_key_ref.validate()?;
        Ok(())
    }
}

impl LocalSettings {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.mountpoint.validate()?;
        self.cache.validate()?;
        self.runtime.validate()?;
        Ok(())
    }
}

impl Mountpoint {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.path.as_os_str().is_empty() {
            return Err(ProtocolError::MissingField("mountpoint.path"));
        }
        Ok(())
    }
}

impl CacheSettings {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.cache_dir.as_os_str().is_empty() {
            return Err(ProtocolError::MissingField("cache.cache_dir"));
        }

        if self.cache_size_mib == 0 {
            return Err(ProtocolError::InvalidField {
                field: "cache.cache_size_mib",
                details: "must be greater than zero".to_string(),
            });
        }

        Ok(())
    }
}

impl RuntimeSettings {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(port) = self.last_local_postgres_port {
            if port == 0 {
                return Err(ProtocolError::InvalidField {
                    field: "runtime.last_local_postgres_port",
                    details: "must be non-zero".to_string(),
                });
            }
        }

        if let Some(port) = self.last_local_s3_port {
            if port == 0 {
                return Err(ProtocolError::InvalidField {
                    field: "runtime.last_local_s3_port",
                    details: "must be non-zero".to_string(),
                });
            }
        }

        Ok(())
    }
}

fn ensure_non_empty(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::MissingField(field));
    }
    Ok(())
}

fn ensure_template_contains(
    field: &'static str,
    template: &str,
    token: &str,
) -> Result<(), ProtocolError> {
    if !template.contains(token) {
        return Err(ProtocolError::InvalidField {
            field,
            details: format!("missing {token} placeholder"),
        });
    }
    Ok(())
}
