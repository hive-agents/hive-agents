use crate::host_key;
use crate::util::{err, take_value, Result};
use chrono::{DateTime, Utc};
use hive_protocol::{
    EndpointMode, JuiceFsConfig, JuiceFsMetaConfig, JuiceFsObjectConfig, MetaEngine, Profile,
    ProfileId, SecretRef, SshConfig, StorageKind, TunnelConfig, PROFILE_VERSION,
};
use std::collections::VecDeque;
use std::fs;
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
        user: "hive".to_string(),
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
    let secret_prefix = format!("keychain:get-hive:profile/{}/", profile_id.0);

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

    let json = if opts.pretty {
        serde_json::to_string_pretty(&profile)
    } else {
        serde_json::to_string(&profile)
    }
    .map_err(|e| err(format!("failed to serialize profile: {}", e)))?;

    if let Some(out) = opts.out.as_ref() {
        fs::write(out, json)
            .map_err(|e| err(format!("failed to write {}: {}", out.display(), e)))?;
        eprintln!("wrote profile to {}", out.display());
    } else {
        println!("{}", json);
    }

    Ok(())
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
