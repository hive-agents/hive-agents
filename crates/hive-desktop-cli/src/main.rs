use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use directories::ProjectDirs;
use hive_desktop_core::{
    pair_envelope, DesktopError, LogLevel, LogLine, LogSource, SecretStore, SupervisorConfig,
    SupervisorHandle,
};
use hive_protocol::{
    CacheSettings, LocalSettings, Mountpoint, PairingEnvelope, Platform, Profile, RuntimeSettings,
};

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug)]
struct CliError(String);

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        CliError(format!("io error: {}", err))
    }
}

impl From<serde_json::Error> for CliError {
    fn from(err: serde_json::Error) -> Self {
        CliError(format!("json error: {}", err))
    }
}

impl From<DesktopError> for CliError {
    fn from(err: DesktopError) -> Self {
        CliError(err.to_string())
    }
}

fn err(message: impl Into<String>) -> CliError {
    CliError(message.into())
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {}", err);
        eprintln!("run 'hive-desktop-cli --help' for usage");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args: VecDeque<String> = env::args().skip(1).collect();
    let Some(cmd) = args.pop_front() else {
        print_usage();
        return Ok(());
    };

    match cmd.as_str() {
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "connect" => {
            if wants_help(&args) {
                print_connect_usage();
                return Ok(());
            }
            let opts = parse_connect_args(&mut args)?;
            run_connect(opts).await
        }
        _ => Err(err(format!("unknown command: {}", cmd))),
    }
}

fn wants_help(args: &VecDeque<String>) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn print_usage() {
    println!("hive-desktop-cli <command> [options]");
    println!();
    println!("commands:");
    println!("  connect           run an end-to-end connect session");
    println!();
    println!("use 'hive-desktop-cli <command> --help' for command options");
}

fn print_connect_usage() {
    println!("hive-desktop-cli connect [options]");
    println!();
    println!("options:");
    println!("  --envelope <PATH|JSON>   pairing envelope json file or inline json");
    println!("  --mountpoint <PATH>      mountpoint path");
    println!("  --cache-dir <PATH>       cache directory (default: ~/.cache/hive/<profile>)");
    println!("  --cache-size <MIB>       cache size in MiB (default: 10240)");
    println!("  --known-hosts <PATH>     known_hosts file (default: ~/.config/hive/known_hosts)");
    println!("  --accept-host-key        fetch and pin the host key automatically");
    println!("  --replace-existing       replace existing device name during pairing");
    println!("  --ssh-path <PATH>        ssh binary path (default: ssh)");
    println!("  --juicefs-path <PATH>    juicefs binary path (default: juicefs)");
    println!("  --log-lines <N>          log lines to keep (default: 200)");
    println!("  --status-interval <N>    status poll seconds (default: 5)");
}

struct ConnectOptions {
    envelope_input: String,
    mountpoint: PathBuf,
    cache_dir: Option<PathBuf>,
    cache_size_mib: u32,
    known_hosts: PathBuf,
    accept_host_key: bool,
    replace_existing: bool,
    ssh_path: PathBuf,
    juicefs_path: PathBuf,
    log_lines: usize,
    status_interval: u64,
}

fn parse_connect_args(args: &mut VecDeque<String>) -> Result<ConnectOptions> {
    let mut envelope_input = None;
    let mut mountpoint = None;
    let mut cache_dir = None;
    let mut cache_size_mib = 10240u32;
    let mut known_hosts = default_known_hosts_path();
    let mut accept_host_key = false;
    let mut replace_existing = false;
    let mut ssh_path = PathBuf::from("ssh");
    let mut juicefs_path = PathBuf::from("juicefs");
    let mut log_lines = 200usize;
    let mut status_interval = 5u64;

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--envelope" => envelope_input = Some(take_value(args, "--envelope")?),
            "--mountpoint" => mountpoint = Some(PathBuf::from(take_value(args, "--mountpoint")?)),
            "--cache-dir" => cache_dir = Some(PathBuf::from(take_value(args, "--cache-dir")?)),
            "--cache-size" => {
                let value = take_value(args, "--cache-size")?;
                cache_size_mib = value
                    .parse::<u32>()
                    .map_err(|_| err("--cache-size must be an integer"))?;
            }
            "--known-hosts" => known_hosts = PathBuf::from(take_value(args, "--known-hosts")?),
            "--accept-host-key" => accept_host_key = true,
            "--replace-existing" => replace_existing = true,
            "--ssh-path" => ssh_path = PathBuf::from(take_value(args, "--ssh-path")?),
            "--juicefs-path" => juicefs_path = PathBuf::from(take_value(args, "--juicefs-path")?),
            "--log-lines" => {
                let value = take_value(args, "--log-lines")?;
                log_lines = value
                    .parse::<usize>()
                    .map_err(|_| err("--log-lines must be an integer"))?;
            }
            "--status-interval" => {
                let value = take_value(args, "--status-interval")?;
                status_interval = value
                    .parse::<u64>()
                    .map_err(|_| err("--status-interval must be an integer"))?;
            }
            _ => return Err(err(format!("unknown connect flag: {}", arg))),
        }
    }

    let envelope_input = envelope_input.ok_or_else(|| err("--envelope is required"))?;
    let mountpoint = mountpoint.ok_or_else(|| err("--mountpoint is required"))?;

    Ok(ConnectOptions {
        envelope_input,
        mountpoint,
        cache_dir,
        cache_size_mib,
        known_hosts,
        accept_host_key,
        replace_existing,
        ssh_path,
        juicefs_path,
        log_lines,
        status_interval,
    })
}

fn take_value(args: &mut VecDeque<String>, flag: &str) -> Result<String> {
    args.pop_front()
        .ok_or_else(|| err(format!("missing value for {}", flag)))
}

async fn run_connect(opts: ConnectOptions) -> Result<()> {
    let envelope = load_envelope(&opts.envelope_input)?;
    let profile = envelope.profile.clone();
    let pairing = pair_envelope(&envelope, None, opts.replace_existing).await?;
    let cache_dir = opts
        .cache_dir
        .unwrap_or_else(|| default_cache_dir(&profile));
    let local_settings =
        build_local_settings(&profile, &opts.mountpoint, &cache_dir, opts.cache_size_mib)?;

    let mut secrets = pairing.response.secrets;
    secrets.insert(
        profile.ssh.identity_key_ref.as_str().to_string(),
        pairing.device_private_key,
    );
    persist_pairing_artifacts(&profile, &envelope, &local_settings, &secrets)?;

    ensure_known_hosts(&profile, &opts.known_hosts, opts.accept_host_key)?;
    let secrets = MapSecretStore::new(secrets);
    let mut config = SupervisorConfig::default();
    config.known_hosts_path = opts.known_hosts.clone();
    config.ssh_path = opts.ssh_path.clone();
    config.juicefs_path = opts.juicefs_path.clone();
    config.log_capacity = opts.log_lines.max(1);

    let handle = SupervisorHandle::spawn(profile, local_settings, Arc::new(secrets), config);
    let mut interval = tokio::time::interval(Duration::from_secs(opts.status_interval.max(1)));
    let mut last_log_count = 0usize;
    let mut last_status = String::new();

    let mut connect_future = Box::pin(handle.connect());
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("disconnect: requested");
                break;
            }
            result = &mut connect_future => {
                if let Err(err) = result {
                    let logs = handle.logs_tail(opts.log_lines);
                    if !logs.is_empty() {
                        eprintln!("connect failed; recent logs:");
                        for log in logs {
                            print_log(&log);
                        }
                    }
                    return Err(err.into());
                }
                println!("connect: started");
                break;
            }
            _ = interval.tick() => {
                let status = handle.status();
                let status_line = format_status(&status);
                if status_line != last_status {
                    println!("{}", status_line);
                    last_status = status_line;
                }

                let logs = handle.logs_tail(opts.log_lines);
                if logs.len() < last_log_count {
                    last_log_count = 0;
                }
                for log in logs.iter().skip(last_log_count) {
                    print_log(log);
                }
                last_log_count = logs.len();
            }
        }
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("disconnect: requested");
                break;
            }
            _ = interval.tick() => {
                let status = handle.status();
                let status_line = format_status(&status);
                if status_line != last_status {
                    println!("{}", status_line);
                    last_status = status_line;
                }

                let logs = handle.logs_tail(opts.log_lines);
                if logs.len() < last_log_count {
                    last_log_count = 0;
                }
                for log in logs.iter().skip(last_log_count) {
                    print_log(log);
                }
                last_log_count = logs.len();
            }
        }
    }

    if let Err(err) = handle.disconnect().await {
        eprintln!("disconnect failed: {}", err);
        handle.force_disconnect().await?;
    }

    Ok(())
}

fn load_envelope(input: &str) -> Result<PairingEnvelope> {
    let content = if input == "-" {
        read_stdin()?
    } else {
        let path = Path::new(input);
        if path.exists() {
            fs::read_to_string(path)
                .map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?
        } else {
            input.to_string()
        }
    };
    let envelope: PairingEnvelope = serde_json::from_str(&content)
        .map_err(|e| {
            err(format!(
                "invalid pairing envelope json (expected file path, '-' for stdin, or inline json): {}",
                e
            ))
        })?;
    envelope
        .profile
        .validate()
        .map_err(|e| err(format!("profile invalid: {}", e)))?;
    Ok(envelope)
}

fn read_stdin() -> Result<String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|e| err(format!("failed to read stdin: {}", e)))?;
    Ok(buffer)
}

struct CliPaths {
    profiles_dir: PathBuf,
    settings_dir: PathBuf,
    secrets_dir: PathBuf,
    pairing_dir: PathBuf,
    known_hosts_path: PathBuf,
    cache_dir: PathBuf,
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "hive-agents", "hive").or_else(|| ProjectDirs::from("", "", "hive"))
}

fn resolve_paths() -> Result<CliPaths> {
    let (data_dir, config_dir, cache_dir) = if let Some(dirs) = project_dirs() {
        (
            dirs.data_dir().to_path_buf(),
            dirs.config_dir().to_path_buf(),
            dirs.cache_dir().to_path_buf(),
        )
    } else {
        let base = env::current_dir()
            .map_err(|e| err(format!("failed to resolve data dir: {}", e)))?
            .join(".hive");
        (base.clone(), base.clone(), base.join("cache"))
    };

    let profiles_dir = data_dir.join("profiles");
    let settings_dir = data_dir.join("settings");
    let secrets_dir = data_dir.join("secrets");
    let pairing_dir = data_dir.join("pairing");
    let known_hosts_path = config_dir.join("known_hosts");

    ensure_dir(&profiles_dir, 0o700)?;
    ensure_dir(&settings_dir, 0o700)?;
    ensure_dir(&secrets_dir, 0o700)?;
    ensure_dir(&pairing_dir, 0o700)?;
    ensure_dir(&cache_dir, 0o700)?;
    if let Some(parent) = known_hosts_path.parent() {
        ensure_dir(parent, 0o700)?;
    }

    Ok(CliPaths {
        profiles_dir,
        settings_dir,
        secrets_dir,
        pairing_dir,
        known_hosts_path,
        cache_dir,
    })
}

fn ensure_dir(path: &Path, mode: u32) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| err(format!("failed to create {}: {}", path.display(), e)))?;
    set_permissions(path, mode)?;
    Ok(())
}

fn set_permissions(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms)
            .map_err(|e| err(format!("failed to set permissions on {}: {}", path.display(), e)))?;
    }
    Ok(())
}

fn build_local_settings(
    profile: &Profile,
    mountpoint: &Path,
    cache_dir: &Path,
    cache_size_mib: u32,
) -> Result<LocalSettings> {
    let local_settings = LocalSettings {
        profile_id: profile.profile_id.clone(),
        mountpoint: Mountpoint {
            platform: detect_platform(),
            path: mountpoint.to_path_buf(),
        },
        cache: CacheSettings {
            cache_dir: cache_dir.to_path_buf(),
            cache_size_mib,
        },
        runtime: RuntimeSettings::default(),
    };

    local_settings
        .validate()
        .map_err(|e| err(format!("local settings invalid: {}", e)))?;
    Ok(local_settings)
}

fn detect_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::Macos
    }
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        Platform::Linux
    }
}

fn default_cache_dir(profile: &Profile) -> PathBuf {
    let suffix = profile.profile_id.0.to_string();
    if let Some(dirs) = project_dirs() {
        return dirs.cache_dir().join(&suffix);
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".cache/hive").join(suffix);
    }
    PathBuf::from("cache").join(suffix)
}

fn default_known_hosts_path() -> PathBuf {
    if let Some(dirs) = project_dirs() {
        return dirs.config_dir().join("known_hosts");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".config/hive/known_hosts");
    }
    PathBuf::from("known_hosts")
}

fn persist_pairing_artifacts(
    profile: &Profile,
    envelope: &PairingEnvelope,
    local_settings: &LocalSettings,
    secrets: &BTreeMap<String, String>,
) -> Result<()> {
    let paths = resolve_paths()?;
    write_profile(&paths, profile)?;
    write_local_settings(&paths, profile, local_settings)?;
    write_pairing_envelope(&paths, profile, envelope)?;
    merge_and_write_secrets(&paths, profile, secrets)?;
    Ok(())
}

fn profile_path(paths: &CliPaths, profile: &Profile) -> PathBuf {
    paths
        .profiles_dir
        .join(format!("{}.json", profile.profile_id.0))
}

fn settings_path(paths: &CliPaths, profile: &Profile) -> PathBuf {
    paths
        .settings_dir
        .join(format!("{}.json", profile.profile_id.0))
}

fn secrets_path(paths: &CliPaths, profile: &Profile) -> PathBuf {
    paths
        .secrets_dir
        .join(format!("{}.json", profile.profile_id.0))
}

fn pairing_path(paths: &CliPaths, profile: &Profile) -> PathBuf {
    paths
        .pairing_dir
        .join(format!("{}.json", profile.profile_id.0))
}

fn write_profile(paths: &CliPaths, profile: &Profile) -> Result<()> {
    let path = profile_path(paths, profile);
    let json = serde_json::to_string_pretty(profile)
        .map_err(|e| err(format!("failed to serialize profile: {}", e)))?;
    fs::write(&path, json)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn write_local_settings(
    paths: &CliPaths,
    profile: &Profile,
    settings: &LocalSettings,
) -> Result<()> {
    let path = settings_path(paths, profile);
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| err(format!("failed to serialize settings: {}", e)))?;
    fs::write(&path, json)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn write_pairing_envelope(
    paths: &CliPaths,
    profile: &Profile,
    envelope: &PairingEnvelope,
) -> Result<()> {
    let path = pairing_path(paths, profile);
    let json = serde_json::to_string_pretty(envelope)
        .map_err(|e| err(format!("failed to serialize pairing envelope: {}", e)))?;
    fs::write(&path, json)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    set_permissions(&path, 0o600)?;
    Ok(())
}

fn merge_and_write_secrets(
    paths: &CliPaths,
    profile: &Profile,
    updates: &BTreeMap<String, String>,
) -> Result<()> {
    let path = secrets_path(paths, profile);
    let mut secrets = read_secrets(&path)?;
    for (key, value) in updates {
        secrets.insert(key.to_string(), value.to_string());
    }
    write_secrets(&path, &secrets)
}

fn read_secrets(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content =
        fs::read_to_string(path).map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?;
    serde_json::from_str(&content)
        .map_err(|e| err(format!("failed to parse secrets: {}", e)))
}

fn write_secrets(path: &Path, secrets: &BTreeMap<String, String>) -> Result<()> {
    let json = serde_json::to_string_pretty(secrets)
        .map_err(|e| err(format!("failed to serialize secrets: {}", e)))?;
    fs::write(path, json)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    set_permissions(path, 0o600)?;
    Ok(())
}

fn ensure_known_hosts(profile: &Profile, path: &Path, accept: bool) -> Result<()> {
    if host_present_in_known_hosts(profile, path)? {
        return Ok(());
    }
    if !accept {
        return Ok(());
    }

    let keyscan = run_keyscan(&profile.ssh.host, profile.ssh.port)?;
    match keyscan_fingerprints(&keyscan) {
        Ok(fingerprints) => {
            if !fingerprints
                .iter()
                .any(|fp| fp == &profile.ssh.host_key_fingerprint_sha256)
            {
                eprintln!(
                    "warning: host key fingerprint mismatch (expected {})",
                    profile.ssh.host_key_fingerprint_sha256
                );
            }
        }
        Err(err) => {
            eprintln!("warning: failed to parse host key fingerprint: {}", err);
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let needs_newline = match fs::read(path) {
        Ok(bytes) => bytes.last().map(|b| *b != b'\n').unwrap_or(false),
        Err(_) => false,
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if needs_newline {
        file.write_all(b"\n")?;
    }
    file.write_all(keyscan.as_bytes())?;
    Ok(())
}

fn host_present_in_known_hosts(profile: &Profile, path: &Path) -> Result<bool> {
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(false);
    };
    let host = profile.ssh.host.as_str();
    let host_port = format!("[{}]:{}", host, profile.ssh.port);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with(host) || trimmed.starts_with(&host_port) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn run_keyscan(host: &str, port: u16) -> Result<String> {
    let output = Command::new("ssh-keyscan")
        .arg("-p")
        .arg(port.to_string())
        .arg("-T")
        .arg("5")
        .arg(host)
        .output()
        .map_err(|e| err(format!("failed to run ssh-keyscan: {}", e)))?;

    if !output.status.success() {
        return Err(err("ssh-keyscan failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        lines.push(trimmed);
    }
    if lines.is_empty() {
        return Err(err("ssh-keyscan returned no host keys"));
    }
    Ok(lines.join("\n") + "\n")
}

fn keyscan_fingerprints(keyscan: &str) -> Result<Vec<String>> {
    let mut child = Command::new("ssh-keygen")
        .arg("-lf")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| err(format!("failed to run ssh-keygen: {}", e)))?;

    {
        let mut stdin = child.stdin.take().ok_or_else(|| err("ssh-keygen stdin unavailable"))?;
        stdin.write_all(keyscan.as_bytes())?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| err(format!("ssh-keygen failed: {}", e)))?;
    if !output.status.success() {
        return Err(err("ssh-keygen returned error"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut fingerprints = Vec::new();
    for line in stdout.lines() {
        for part in line.split_whitespace() {
            if part.starts_with("SHA256:") {
                fingerprints.push(part.to_string());
            }
        }
    }
    if fingerprints.is_empty() {
        return Err(err("failed to read host key fingerprints"));
    }
    Ok(fingerprints)
}

fn format_status(status: &hive_desktop_core::Status) -> String {
    let mountpoint = status
        .mountpoint
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_string());
    let ports = format_ports(&status.local_ports);
    let error = status
        .last_error
        .as_ref()
        .map(|value| value.as_str())
        .unwrap_or("-");
    format!("status: state={:?} mountpoint={} ports={} error={}", status.state, mountpoint, ports, error)
}

fn format_ports(ports: &BTreeMap<String, u16>) -> String {
    if ports.is_empty() {
        return "-".to_string();
    }
    let mut parts = Vec::new();
    for (name, port) in ports {
        parts.push(format!("{}={}", name, port));
    }
    parts.join(",")
}

fn print_log(log: &LogLine) {
    let level = match log.level {
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };
    let source = match log.source {
        LogSource::Supervisor => "supervisor",
        LogSource::Tunnel => "tunnel",
        LogSource::Mount => "mount",
    };
    println!(
        "[{}] {} {}: {}",
        log.timestamp.to_rfc3339(),
        source,
        level,
        log.message
    );
}

struct MapSecretStore {
    secrets: BTreeMap<String, String>,
}

impl MapSecretStore {
    fn new(secrets: BTreeMap<String, String>) -> Self {
        Self { secrets }
    }
}

impl SecretStore for MapSecretStore {
    fn resolve_text(&self, secret: &hive_protocol::SecretRef) -> hive_desktop_core::Result<String> {
        self.secrets
            .get(secret.as_str())
            .cloned()
            .ok_or_else(|| DesktopError::SecretNotFound(secret.as_str().to_string()))
    }

    fn resolve_path(&self, secret: &hive_protocol::SecretRef) -> hive_desktop_core::Result<PathBuf> {
        Err(DesktopError::SecretNotFound(format!(
            "path resolution not supported: {}",
            secret.as_str()
        )))
    }
}
