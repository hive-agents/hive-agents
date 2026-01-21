use std::collections::{BTreeMap, HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use hive_desktop_core::{
    DesktopError, LogLine, LogSource, LogLevel, SecretStore, SupervisorConfig, SupervisorHandle,
};
use hive_protocol::{CacheSettings, LocalSettings, Mountpoint, Platform, Profile, RuntimeSettings};

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
        "secrets-template" => {
            if wants_help(&args) {
                print_secrets_usage();
                return Ok(());
            }
            let profile_path = parse_profile_only(&mut args)?;
            run_secrets_template(&profile_path)
        }
        "keygen" => {
            if wants_help(&args) {
                print_keygen_usage();
                return Ok(());
            }
            let opts = parse_keygen_args(&mut args)?;
            run_keygen(opts)
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
    println!("  secrets-template  print the required secrets JSON map");
    println!("  keygen            generate an ed25519 device key");
    println!();
    println!("use 'hive-desktop-cli <command> --help' for command options");
}

fn print_connect_usage() {
    println!("hive-desktop-cli connect [options]");
    println!();
    println!("options:");
    println!("  --profile <PATH>         profile json file");
    println!("  --secrets <PATH>         secrets json file (secret_ref -> value)");
    println!("  --mountpoint <PATH>      mountpoint path");
    println!("  --cache-dir <PATH>       cache directory (default: ~/.cache/hive-agents/<profile>)");
    println!("  --cache-size <MIB>       cache size in MiB (default: 10240)");
    println!("  --known-hosts <PATH>     known_hosts file (default: ~/.config/hive-agents/known_hosts)");
    println!("  --accept-host-key        fetch and pin the host key automatically");
    println!("  --ssh-path <PATH>        ssh binary path (default: ssh)");
    println!("  --juicefs-path <PATH>    juicefs binary path (default: juicefs)");
    println!("  --log-lines <N>          log lines to keep (default: 200)");
    println!("  --status-interval <N>    status poll seconds (default: 5)");
}

fn print_secrets_usage() {
    println!("hive-desktop-cli secrets-template --profile <PATH>");
    println!();
    println!("prints a JSON map of secret_ref -> empty string");
}

fn print_keygen_usage() {
    println!("hive-desktop-cli keygen [options]");
    println!();
    println!("options:");
    println!("  --out <PATH>      output key path (private key)");
    println!("  --comment <TEXT>  ssh key comment");
}

struct ConnectOptions {
    profile_path: PathBuf,
    secrets_path: PathBuf,
    mountpoint: PathBuf,
    cache_dir: Option<PathBuf>,
    cache_size_mib: u32,
    known_hosts: PathBuf,
    accept_host_key: bool,
    ssh_path: PathBuf,
    juicefs_path: PathBuf,
    log_lines: usize,
    status_interval: u64,
}

struct KeygenOptions {
    out_path: PathBuf,
    comment: Option<String>,
}

fn parse_connect_args(args: &mut VecDeque<String>) -> Result<ConnectOptions> {
    let mut profile_path = None;
    let mut secrets_path = None;
    let mut mountpoint = None;
    let mut cache_dir = None;
    let mut cache_size_mib = 10240u32;
    let mut known_hosts = default_known_hosts_path();
    let mut accept_host_key = false;
    let mut ssh_path = PathBuf::from("ssh");
    let mut juicefs_path = PathBuf::from("juicefs");
    let mut log_lines = 200usize;
    let mut status_interval = 5u64;

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--profile" => profile_path = Some(PathBuf::from(take_value(args, "--profile")?)),
            "--secrets" => secrets_path = Some(PathBuf::from(take_value(args, "--secrets")?)),
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

    let profile_path = profile_path.ok_or_else(|| err("--profile is required"))?;
    let secrets_path = secrets_path.ok_or_else(|| err("--secrets is required"))?;
    let mountpoint = mountpoint.ok_or_else(|| err("--mountpoint is required"))?;

    Ok(ConnectOptions {
        profile_path,
        secrets_path,
        mountpoint,
        cache_dir,
        cache_size_mib,
        known_hosts,
        accept_host_key,
        ssh_path,
        juicefs_path,
        log_lines,
        status_interval,
    })
}

fn parse_profile_only(args: &mut VecDeque<String>) -> Result<PathBuf> {
    let mut profile_path = None;
    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--profile" => profile_path = Some(PathBuf::from(take_value(args, "--profile")?)),
            _ => return Err(err(format!("unknown flag: {}", arg))),
        }
    }
    profile_path.ok_or_else(|| err("--profile is required"))
}

fn parse_keygen_args(args: &mut VecDeque<String>) -> Result<KeygenOptions> {
    let mut out_path = None;
    let mut comment = None;
    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--out" => out_path = Some(PathBuf::from(take_value(args, "--out")?)),
            "--comment" => comment = Some(take_value(args, "--comment")?),
            _ => return Err(err(format!("unknown keygen flag: {}", arg))),
        }
    }

    let out_path = out_path.ok_or_else(|| err("--out is required"))?;
    Ok(KeygenOptions { out_path, comment })
}

fn take_value(args: &mut VecDeque<String>, flag: &str) -> Result<String> {
    args.pop_front()
        .ok_or_else(|| err(format!("missing value for {}", flag)))
}

async fn run_connect(opts: ConnectOptions) -> Result<()> {
    let profile = load_profile(&opts.profile_path)?;
    let cache_dir = opts
        .cache_dir
        .unwrap_or_else(|| default_cache_dir(&profile));
    let local_settings =
        build_local_settings(&profile, &opts.mountpoint, &cache_dir, opts.cache_size_mib)?;

    ensure_known_hosts(&profile, &opts.known_hosts, opts.accept_host_key)?;

    let secrets = FileSecretStore::from_path(&opts.secrets_path)?;
    let mut config = SupervisorConfig::default();
    config.known_hosts_path = opts.known_hosts.clone();
    config.ssh_path = opts.ssh_path.clone();
    config.juicefs_path = opts.juicefs_path.clone();
    config.log_capacity = opts.log_lines.max(1);

    let handle =
        SupervisorHandle::spawn(profile, local_settings, Arc::new(secrets), config);
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

fn run_secrets_template(profile_path: &Path) -> Result<()> {
    let profile = load_profile(profile_path)?;
    let mut map = HashMap::new();
    map.insert(profile.ssh.identity_key_ref.as_str().to_string(), "");
    map.insert(profile.juicefs.meta.password_ref.as_str().to_string(), "");
    map.insert(profile.juicefs.object.access_key_ref.as_str().to_string(), "");
    map.insert(profile.juicefs.object.secret_key_ref.as_str().to_string(), "");
    let json = serde_json::to_string_pretty(&map)?;
    println!("{}", json);
    Ok(())
}

fn run_keygen(opts: KeygenOptions) -> Result<()> {
    if opts.out_path.exists() {
        return Err(err("output key path already exists"));
    }

    let mut cmd = Command::new("ssh-keygen");
    cmd.arg("-t").arg("ed25519").arg("-f").arg(&opts.out_path).arg("-N").arg("");
    if let Some(comment) = opts.comment.as_ref() {
        cmd.arg("-C").arg(comment);
    }
    let status = cmd.status().map_err(|e| err(format!("ssh-keygen failed: {}", e)))?;
    if !status.success() {
        return Err(err("ssh-keygen exited with error"));
    }

    let pubkey_path = opts.out_path.with_extension("pub");
    let pubkey = fs::read_to_string(&pubkey_path)
        .map_err(|e| err(format!("failed to read {}: {}", pubkey_path.display(), e)))?;
    print!("{}", pubkey);
    Ok(())
}

fn load_profile(path: &Path) -> Result<Profile> {
    let content = fs::read_to_string(path)
        .map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?;
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let profile_value = value.get("profile").cloned().unwrap_or(value);
    let profile: Profile = serde_json::from_value(profile_value)?;
    profile
        .validate()
        .map_err(|e| err(format!("profile invalid: {}", e)))?;
    Ok(profile)
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
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".cache/hive-agents").join(suffix);
    }
    PathBuf::from("cache").join(suffix)
}

fn default_known_hosts_path() -> PathBuf {
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".config/hive-agents/known_hosts");
    }
    PathBuf::from("known_hosts")
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

struct FileSecretStore {
    secrets: HashMap<String, String>,
}

impl FileSecretStore {
    fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?;
        let secrets: HashMap<String, String> = serde_json::from_str(&content)?;
        Ok(Self { secrets })
    }
}

impl SecretStore for FileSecretStore {
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
