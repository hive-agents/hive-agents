use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, ConnectInfo, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use chrono::{DateTime, Utc};
use hive_protocol::{PairError, PairRequest, PairResponse, ProfileId};
use serde::Deserialize;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
struct Options {
    root: PathBuf,
    listen: String,
    authorized_keys: PathBuf,
    replace_existing: bool,
}

#[derive(Debug, Clone)]
struct EnvConfig {
    postgres_password: String,
    s3_access_key: String,
    s3_secret_key: String,
}

#[derive(Debug, Clone)]
struct AppState {
    otp_dir: PathBuf,
    authorized_keys: PathBuf,
    env: EnvConfig,
    replace_existing: bool,
}

#[derive(Debug, Deserialize)]
struct OtpRecord {
    otp: String,
    profile_id: ProfileId,
    expires_at: DateTime<Utc>,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        eprintln!("run 'hive-core-pairing --help' for usage");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let opts = parse_args()?;
    let env_config = load_env_config(&opts.root)?;
    let otp_dir = opts.root.join("state").join("pairing");
    ensure_dir(&otp_dir)?;

    let state = Arc::new(AppState {
        otp_dir,
        authorized_keys: opts.authorized_keys.clone(),
        env: env_config,
        replace_existing: opts.replace_existing,
    });

    let app = Router::new()
        .route("/hive-pair", post(pair_handler))
        .with_state(state);

    let addr: SocketAddr = opts
        .listen
        .parse()
        .map_err(|_| format!("invalid listen address: {}", opts.listen))?;

    println!("pairing server listening on {}", opts.listen);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("failed to bind {}: {}", opts.listen, e))?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| format!("server error: {e}"))?;
    Ok(())
}

async fn pair_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    payload: std::result::Result<Json<PairRequest>, JsonRejection>,
) -> Response {
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(_) => {
            log_event(
                "warn",
                "pair_reject",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("reason", serde_json::json!("invalid_json")),
                ],
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid json body",
            )
        }
    };
    log_event(
        "info",
        "pair_request",
        &[
            ("remote_addr", serde_json::json!(remote_addr.to_string())),
            ("device_name", serde_json::json!(payload.device_name)),
            ("otp_prefix", serde_json::json!(otp_prefix(&payload.otp))),
            ("pubkey_type", serde_json::json!(pubkey_type(&payload.device_pubkey))),
            ("replace_existing", serde_json::json!(payload.replace_existing)),
        ],
    );
    // TODO: Add rate limiting and attempt tracking (per-IP or per-OTP).
    if let Err(err) = cleanup_expired_otps(&state.otp_dir) {
        log_event(
            "warn",
            "otp_cleanup_failed",
            &[
                ("remote_addr", serde_json::json!(remote_addr.to_string())),
                ("error", serde_json::json!(err)),
            ],
        );
    }
    let otp = payload.otp.trim();
    if otp.is_empty() {
        log_reject(
            remote_addr,
            "otp_missing",
            Some("otp is required"),
            Some(otp),
            Some(&payload.device_name),
        );
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "otp is required",
        );
    }

    if payload.device_name.chars().any(|c| c.is_whitespace()) {
        log_reject(
            remote_addr,
            "device_name_invalid",
            Some("device_name must not contain whitespace"),
            Some(otp),
            Some(&payload.device_name),
        );
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "device_name must not contain whitespace",
        );
    }

    let pubkey = match normalize_pubkey(&payload.device_pubkey) {
        Ok(value) => value,
        Err(message) => {
            log_reject(
                remote_addr,
                "device_pubkey_invalid",
                Some(&message),
                Some(otp),
                Some(&payload.device_name),
            );
            return error_response(StatusCode::BAD_REQUEST, "invalid_request", &message);
        }
    };

    let record_path = state.otp_dir.join(format!("{}.json", otp));
    let record = match load_otp_record(&record_path) {
        Ok(Some(record)) => record,
        Ok(None) => {
            log_reject(
                remote_addr,
                "otp_invalid",
                Some("OTP is invalid, expired, or already used"),
                Some(otp),
                Some(&payload.device_name),
            );
            return error_response(
                StatusCode::UNAUTHORIZED,
                "otp_invalid",
                "OTP is invalid, expired, or already used",
            )
        }
        Err(message) => {
            log_event(
                "error",
                "pair_error",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("reason", serde_json::json!("otp_read_failed")),
                    ("error", serde_json::json!(message)),
                ],
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", &message)
        }
    };

    if record.otp != otp {
        let _ = fs::remove_file(&record_path);
        log_reject(
            remote_addr,
            "otp_invalid",
            Some("OTP is invalid, expired, or already used"),
            Some(otp),
            Some(&payload.device_name),
        );
        return error_response(
            StatusCode::UNAUTHORIZED,
            "otp_invalid",
            "OTP is invalid, expired, or already used",
        );
    }

    if Utc::now() > record.expires_at {
        let _ = fs::remove_file(&record_path);
        log_reject(
            remote_addr,
            "otp_expired",
            Some("OTP is invalid, expired, or already used"),
            Some(otp),
            Some(&payload.device_name),
        );
        return error_response(
            StatusCode::UNAUTHORIZED,
            "otp_invalid",
            "OTP is invalid, expired, or already used",
        );
    }

    if let Err(err) = fs::remove_file(&record_path) {
        if err.kind() == std::io::ErrorKind::NotFound {
            log_reject(
                remote_addr,
                "otp_invalid",
                Some("OTP is invalid, expired, or already used"),
                Some(otp),
                Some(&payload.device_name),
            );
            return error_response(
                StatusCode::UNAUTHORIZED,
                "otp_invalid",
                "OTP is invalid, expired, or already used",
            );
        }
        log_event(
            "error",
            "pair_error",
            &[
                ("remote_addr", serde_json::json!(remote_addr.to_string())),
                ("reason", serde_json::json!("otp_remove_failed")),
                ("error", serde_json::json!(format!(
                    "failed to remove otp file {}: {}",
                    record_path.display(),
                    err
                ))),
            ],
        );
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            &format!("failed to remove otp file {}: {}", record_path.display(), err),
        );
    }

    let existing_keys = match device_keys_for_name(&state.authorized_keys, &payload.device_name) {
        Ok(keys) => keys,
        Err(message) => {
            log_event(
                "error",
                "pair_error",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("reason", serde_json::json!("authorized_keys_read_failed")),
                    ("error", serde_json::json!(message)),
                ],
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", &message);
        }
    };

    let mut should_add = true;
    if !existing_keys.is_empty() {
        if existing_keys.iter().any(|key| key == &pubkey) {
            should_add = false;
            log_event(
                "info",
                "device_exists",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("device_name", serde_json::json!(payload.device_name)),
                ],
            );
        } else if payload.replace_existing {
            if !state.replace_existing {
                log_reject(
                    remote_addr,
                    "replace_not_allowed",
                    Some("server does not allow replacing devices"),
                    Some(otp),
                    Some(&payload.device_name),
                );
                return error_response(
                    StatusCode::FORBIDDEN,
                    "replace_not_allowed",
                    "server does not allow replacing devices",
                );
            }
            if let Err(message) = remove_device_key(&state.authorized_keys, &payload.device_name) {
                log_event(
                    "error",
                    "pair_error",
                    &[
                        ("remote_addr", serde_json::json!(remote_addr.to_string())),
                        ("reason", serde_json::json!("authorized_keys_replace_failed")),
                        ("error", serde_json::json!(message)),
                    ],
                );
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &message,
                );
            }
            log_event(
                "info",
                "device_replace",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("device_name", serde_json::json!(payload.device_name)),
                ],
            );
        } else {
            log_reject(
                remote_addr,
                "device_name_exists",
                Some("device_name already exists"),
                Some(otp),
                Some(&payload.device_name),
            );
            return error_response(
                StatusCode::CONFLICT,
                "device_name_exists",
                "device_name already exists",
            );
        }
    }

    if should_add {
        if let Err(message) = add_device_key(&state.authorized_keys, &payload.device_name, &pubkey) {
            log_event(
                "error",
                "pair_error",
                &[
                    ("remote_addr", serde_json::json!(remote_addr.to_string())),
                    ("reason", serde_json::json!("authorized_keys_write_failed")),
                    ("error", serde_json::json!(message)),
                ],
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", &message);
        }
    }

    let secrets = build_secret_map(&record.profile_id, &state.env);
    let response = PairResponse {
        profile_id: record.profile_id.clone(),
        secrets,
    };
    log_event(
        "info",
        "pair_success",
        &[
            ("remote_addr", serde_json::json!(remote_addr.to_string())),
            ("device_name", serde_json::json!(payload.device_name)),
            ("profile_id", serde_json::json!(record.profile_id.0.to_string())),
        ],
    );

    (StatusCode::OK, Json(response)).into_response()
}

fn parse_args() -> Result<Options> {
    let mut args: VecDeque<String> = env::args().skip(1).collect();
    let mut root = PathBuf::from("/opt/hive-core");
    let mut listen = "127.0.0.1:8081".to_string();
    let mut authorized_keys = PathBuf::from("/home/hivec/.ssh/authorized_keys");
    let mut replace_existing = false;

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(take_value(&mut args, "--root")?),
            "--listen" => listen = take_value(&mut args, "--listen")?,
            "--authorized-keys" => {
                authorized_keys = PathBuf::from(take_value(&mut args, "--authorized-keys")?)
            }
            "--replace-existing" => replace_existing = true,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => return Err(format!("unknown flag: {arg}")),
        }
    }

    Ok(Options {
        root,
        listen,
        authorized_keys,
        replace_existing,
    })
}

fn print_usage() {
    println!("hive-core-pairing [options]");
    println!("");
    println!("options:");
    println!("  --root <PATH>            hive-core root (default: /opt/hive-core)");
    println!("  --listen <ADDR>          listen address (default: 127.0.0.1:8081)");
    println!("  --authorized-keys <PATH> authorized_keys path");
    println!("  --replace-existing       replace existing device key with same name");
}

fn take_value(args: &mut VecDeque<String>, flag: &str) -> Result<String> {
    args.pop_front()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn load_env_config(root: &Path) -> Result<EnvConfig> {
    let env_path = root.join(".env");
    let env = read_env_file(&env_path)?;
    let postgres_password = env
        .get("POSTGRES_PASSWORD")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "POSTGRES_PASSWORD missing from .env".to_string())?;
    let s3_access_key = env
        .get("S3_ACCESS_KEY")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "S3_ACCESS_KEY missing from .env".to_string())?;
    let s3_secret_key = env
        .get("S3_SECRET_KEY")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "S3_SECRET_KEY missing from .env".to_string())?;

    Ok(EnvConfig {
        postgres_password,
        s3_access_key,
        s3_secret_key,
    })
}

fn read_env_file(path: &Path) -> Result<std::collections::HashMap<String, String>> {
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
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

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| format!("failed to create {}: {}", path.display(), e))
}

fn load_otp_record(path: &Path) -> Result<Option<OtpRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let record: OtpRecord = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse otp record: {e}"))?;
    Ok(Some(record))
}

fn cleanup_expired_otps(dir: &Path) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {}", dir.display(), e))?;
    let now = Utc::now();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let record: OtpRecord = match serde_json::from_str(&content) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if now > record.expires_at {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

fn log_reject(
    remote_addr: SocketAddr,
    reason: &str,
    message: Option<&str>,
    otp: Option<&str>,
    device_name: Option<&str>,
) {
    let mut fields = vec![
        ("remote_addr", serde_json::json!(remote_addr.to_string())),
        ("reason", serde_json::json!(reason)),
    ];
    if let Some(message) = message {
        fields.push(("message", serde_json::json!(message)));
    }
    if let Some(otp) = otp {
        fields.push(("otp_prefix", serde_json::json!(otp_prefix(otp))));
    }
    if let Some(device_name) = device_name {
        fields.push(("device_name", serde_json::json!(device_name)));
    }
    log_event("warn", "pair_reject", &fields);
}

fn log_event(level: &str, event: &str, fields: &[(&str, serde_json::Value)]) {
    let mut map = serde_json::Map::new();
    map.insert(
        "ts".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339()),
    );
    map.insert("level".to_string(), serde_json::Value::String(level.to_string()));
    map.insert("event".to_string(), serde_json::Value::String(event.to_string()));
    for (key, value) in fields {
        map.insert((*key).to_string(), value.clone());
    }
    let line = serde_json::Value::Object(map).to_string();
    if level == "warn" || level == "error" {
        eprintln!("{}", line);
    } else {
        println!("{}", line);
    }
}

fn otp_prefix(otp: &str) -> String {
    otp.chars().take(8).collect()
}

fn pubkey_type(pubkey: &str) -> String {
    pubkey
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn normalize_pubkey(pubkey: &str) -> Result<String> {
    let trimmed = pubkey.trim();
    if trimmed.contains('\n') {
        return Err("device_pubkey must be a single line".to_string());
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("device_pubkey is missing key data".to_string());
    }
    if !(parts[0].starts_with("ssh-") || parts[0].starts_with("ecdsa-")) {
        return Err("device_pubkey must start with ssh- or ecdsa-".to_string());
    }
    Ok(format!("{} {}", parts[0], parts[1]))
}

fn device_keys_for_name(authorized_keys: &Path, name: &str) -> Result<Vec<String>> {
    if !authorized_keys.exists() {
        return Ok(Vec::new());
    }
    let token = format!("hive-device={name}");
    let existing = fs::read_to_string(authorized_keys)
        .map_err(|e| format!("failed to read {}: {}", authorized_keys.display(), e))?;
    let mut keys = Vec::new();
    for line in existing.lines() {
        if !line.contains(&token) {
            continue;
        }
        if let Some(key) = extract_pubkey_from_line(line) {
            keys.push(key);
        }
    }
    Ok(keys)
}

fn add_device_key(authorized_keys: &Path, name: &str, pubkey: &str) -> Result<()> {
    ensure_parent_dir(authorized_keys)?;

    let options = format_key_options();
    let line = format!("{} {} hive-device={}\n", options, pubkey, name);

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(authorized_keys)
        .map_err(|e| format!("failed to open {}: {}", authorized_keys.display(), e))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", authorized_keys.display(), e))?;

    set_key_permissions(authorized_keys)?;
    Ok(())
}

fn extract_pubkey_from_line(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    for (idx, part) in parts.iter().enumerate() {
        if part.starts_with("ssh-") || part.starts_with("ecdsa-") {
            if let Some(key_data) = parts.get(idx + 1) {
                return Some(format!("{} {}", part, key_data));
            }
            return None;
        }
    }
    None
}

fn remove_device_key(authorized_keys: &Path, name: &str) -> Result<()> {
    if !authorized_keys.exists() {
        return Ok(());
    }
    let token = format!("hive-device={name}");
    let existing = fs::read_to_string(authorized_keys)
        .map_err(|e| format!("failed to read {}: {}", authorized_keys.display(), e))?;
    let mut kept = String::new();
    let mut removed = false;
    for line in existing.lines() {
        if line.contains(&token) {
            removed = true;
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if removed {
        fs::write(authorized_keys, kept)
            .map_err(|e| format!("failed to write {}: {}", authorized_keys.display(), e))?;
        set_key_permissions(authorized_keys)?;
    }
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    Ok(())
}

fn format_key_options() -> String {
    let options = vec![
        "no-pty".to_string(),
        "no-agent-forwarding".to_string(),
        "no-X11-forwarding".to_string(),
        "permitopen=\"127.0.0.1:5432\"".to_string(),
        "permitopen=\"127.0.0.1:8333\"".to_string(),
    ];
    options.join(",")
}

fn set_key_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .map_err(|e| format!("failed to set permissions on {}: {}", path.display(), e))?;
        if let Some(parent) = path.parent() {
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(parent, perms).ok();
        }
    }
    Ok(())
}

fn build_secret_map(profile_id: &ProfileId, env: &EnvConfig) -> BTreeMap<String, String> {
    let prefix = format!("keychain:hive-agents:profile/{}/", profile_id.0);
    let mut secrets = BTreeMap::new();
    secrets.insert(format!("{}meta_password", prefix), env.postgres_password.clone());
    secrets.insert(format!("{}s3_access_key", prefix), env.s3_access_key.clone());
    secrets.insert(format!("{}s3_secret_key", prefix), env.s3_secret_key.clone());
    secrets
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = PairError {
        error: code.to_string(),
        message: message.to_string(),
    };
    (status, Json(body)).into_response()
}
