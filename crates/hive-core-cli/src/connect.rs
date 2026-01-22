use crate::host_key;
use crate::util::{err, take_value, Result};
use chrono::{DateTime, Duration, Utc};
use hive_protocol::{
    EndpointMode, JuiceFsConfig, JuiceFsMetaConfig, JuiceFsObjectConfig, MetaEngine, PairingEnvelope,
    Profile, ProfileId, SecretRef, SshConfig, StorageKind, TunnelConfig, PROFILE_VERSION,
};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub root: PathBuf,
    pub host: Option<String>,
    pub port: u16,
    pub user: String,
    pub display_name: String,
    pub out: Option<PathBuf>,
    pub pretty: bool,
    pub fingerprint: Option<String>,
}

pub fn parse_args(args: &mut VecDeque<String>) -> Result<ConnectOptions> {
    let mut opts = ConnectOptions {
        root: PathBuf::from("/opt/hive-core"),
        host: None,
        port: 22,
        user: "hivec".to_string(),
        display_name: "Hive".to_string(),
        out: None,
        pretty: false,
        fingerprint: None,
    };

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--root" => opts.root = PathBuf::from(take_value(args, "--root")?),
            "--host" => opts.host = Some(take_value(args, "--host")?),
            "--port" => {
                let value = take_value(args, "--port")?;
                opts.port = value.parse::<u16>().map_err(|_| err("--port must be a number"))?;
            }
            "--user" => opts.user = take_value(args, "--user")?,
            "--display-name" => opts.display_name = take_value(args, "--display-name")?,
            "--out" => opts.out = Some(PathBuf::from(take_value(args, "--out")?)),
            "--pretty" => opts.pretty = true,
            "--fingerprint" => opts.fingerprint = Some(take_value(args, "--fingerprint")?),
            _ => return Err(err(format!("unknown connect flag: {}", arg))),
        }
    }

    Ok(opts)
}

pub fn run(opts: ConnectOptions) -> Result<()> {
    let env_path = opts.root.join(".env");
    if !env_path.exists() {
        return Err(err(format!(
            "missing {} (run hive-core install first)",
            env_path.display()
        )));
    }

    let host = match opts.host.or_else(|| std::env::var("HIVE_HOST").ok()) {
        Some(value) => value,
        None => return Err(err("--host is required (or set HIVE_HOST)")),
    };
    let host = host.trim().to_string();
    if host.is_empty() {
        return Err(err("--host must be non-empty"));
    }

    let env = read_env_file(&env_path)?;
    let postgres_user = env
        .get("POSTGRES_USER")
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "juicefs".to_string());
    let postgres_db = env
        .get("POSTGRES_DB")
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "juicefs_meta".to_string());
    let bucket = env
        .get("HIVE_BUCKET")
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hive".to_string());
    let volume = env
        .get("HIVE_VOLUME")
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "hive".to_string());

    let fingerprint = if let Some(value) = opts.fingerprint.clone() {
        value
    } else {
        host_key::scan_fingerprint(&host, opts.port)?
    };

    let profile_id = ProfileId(Uuid::new_v4());
    let created_at: DateTime<Utc> = Utc::now();
    let secret_prefix = format!("keychain:hive-agents:profile/{}/", profile_id.0);

    let profile = Profile {
        profile_version: PROFILE_VERSION,
        profile_id: profile_id.clone(),
        display_name: opts.display_name,
        created_at,
        ssh: SshConfig {
            host,
            port: opts.port,
            user: opts.user,
            host_key_fingerprint_sha256: fingerprint,
            identity_key_ref: SecretRef(format!("{}ssh_ed25519", secret_prefix)),
        },
        tunnels: vec![
            TunnelConfig {
                name: "postgres".to_string(),
                remote_host: "127.0.0.1".to_string(),
                remote_port: 5432,
                local_bind_host: "127.0.0.1".to_string(),
                local_port: 0,
            },
            TunnelConfig {
                name: "s3".to_string(),
                remote_host: "127.0.0.1".to_string(),
                remote_port: 8333,
                local_bind_host: "127.0.0.1".to_string(),
                local_port: 0,
            },
        ],
        juicefs: JuiceFsConfig {
            volume_name: volume.clone(),
            meta: JuiceFsMetaConfig {
                engine: MetaEngine::Postgres,
                user: postgres_user.clone(),
                database: postgres_db.clone(),
                password_ref: SecretRef(format!("{}meta_password", secret_prefix)),
                dsn_template: format!(
                    "postgres://{}@127.0.0.1:{{local_postgres_port}}/{}?sslmode=disable",
                    postgres_user, postgres_db
                ),
            },
            object: JuiceFsObjectConfig {
                storage: StorageKind::S3,
                bucket_name: bucket.clone(),
                endpoint_mode: EndpointMode::Tunneled,
                bucket_url_template: format!(
                    "http://127.0.0.1:{{local_s3_port}}/{}",
                    bucket
                ),
                access_key_ref: SecretRef(format!("{}s3_access_key", secret_prefix)),
                secret_key_ref: SecretRef(format!("{}s3_secret_key", secret_prefix)),
            },
        },
    };

    profile
        .validate()
        .map_err(|e| err(format!("generated profile invalid: {}", e)))?;

    let otp = generate_hex_secret(32)?;
    let expires_at = Utc::now() + Duration::minutes(5);
    write_otp_record(&opts.root, &otp, &profile_id, expires_at)?;
    // TODO: Allow overriding the pairing host if SSH host differs.
    let pair_url = pair_url_for_host(&profile.ssh.host);

    let envelope = PairingEnvelope {
        profile,
        otp,
        otp_expires_at: expires_at,
        pair_url,
    };

    let json = if opts.pretty {
        serde_json::to_string_pretty(&envelope)
    } else {
        serde_json::to_string(&envelope)
    }
    .map_err(|e| err(format!("failed to serialize envelope: {}", e)))?;

    if let Some(out) = opts.out.as_ref() {
        fs::write(out, json)
            .map_err(|e| err(format!("failed to write {}: {}", out.display(), e)))?;
        eprintln!("wrote profile to {}", out.display());
    } else {
        println!("{}", json);
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct OtpRecord {
    otp: String,
    profile_id: ProfileId,
    expires_at: DateTime<Utc>,
}

fn write_otp_record(
    root: &Path,
    otp: &str,
    profile_id: &ProfileId,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    let dir = root.join("state").join("pairing");
    fs::create_dir_all(&dir)
        .map_err(|e| err(format!("failed to create {}: {}", dir.display(), e)))?;
    let path = dir.join(format!("{}.json", otp));
    if path.exists() {
        return Err(err("otp collision; retry"));
    }

    let record = OtpRecord {
        otp: otp.to_string(),
        profile_id: profile_id.clone(),
        expires_at,
    };
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    let payload = serde_json::to_vec_pretty(&record)
        .map_err(|e| err(format!("failed to serialize otp record: {}", e)))?;
    file.write_all(&payload)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&path, perms).ok();
    }

    Ok(())
}

fn generate_hex_secret(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    let mut file =
        fs::File::open("/dev/urandom").map_err(|e| err(format!("failed to open /dev/urandom: {}", e)))?;
    file.read_exact(&mut buffer)
        .map_err(|e| err(format!("failed to read /dev/urandom: {}", e)))?;
    let mut output = String::with_capacity(bytes * 2);
    for byte in buffer {
        output.push_str(&format!("{:02x}", byte));
    }
    Ok(output)
}

fn pair_url_for_host(host: &str) -> String {
    if is_localhost(host) {
        format!("http://{}:8081/hive-pair", host)
    } else {
        format!("https://{}/hive-pair", host)
    }
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn read_env_file(path: &Path) -> Result<std::collections::HashMap<String, String>> {
    let contents = fs::read_to_string(path)
        .map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?;
    let mut map = std::collections::HashMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn pair_url_for_localhost_uses_http_hive_pair() {
        let url = pair_url_for_host("localhost");
        assert_eq!(url, "http://localhost:8081/hive-pair");
    }

    #[test]
    fn pair_url_for_public_host_uses_https_hive_pair() {
        let url = pair_url_for_host("example.com");
        assert_eq!(url, "https://example.com/hive-pair");
    }

    #[test]
    fn parse_args_defaults_user_to_hivec() {
        let mut args = VecDeque::new();
        let opts = parse_args(&mut args).expect("parse_args should succeed");
        assert_eq!(opts.user, "hivec");
    }
}
