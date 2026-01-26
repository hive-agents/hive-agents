use crate::connect;
use crate::util::{err, take_value, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde_json::json;

const COMPOSE_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../infra/hive-core/compose.template.yml"
));

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub root: PathBuf,
    pub force: bool,
    pub skip_up: bool,
    pub skip_format: bool,
    pub skip_connect: bool,
    pub skip_ssh_user: bool,
    pub replace_existing: bool,
    pub bucket: String,
    pub volume: String,
    pub postgres_user: String,
    pub postgres_db: String,
    pub wait_seconds: u64,
    pub pairing_binary: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct EnvConfig {
    postgres_user: String,
    postgres_db: String,
    postgres_password: String,
    s3_access_key: String,
    s3_secret_key: String,
    bucket: String,
    volume: String,
}

pub fn parse_args(args: &mut VecDeque<String>) -> Result<InstallOptions> {
    let mut opts = InstallOptions {
        root: PathBuf::from("/opt/hive-core"),
        force: false,
        skip_up: false,
        skip_format: false,
        skip_connect: false,
        skip_ssh_user: false,
        replace_existing: false,
        bucket: "hive".to_string(),
        volume: "hive".to_string(),
        postgres_user: "juicefs".to_string(),
        postgres_db: "juicefs_meta".to_string(),
        wait_seconds: 60,
        pairing_binary: None,
    };

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--root" => opts.root = PathBuf::from(take_value(args, "--root")?),
            "--force" => opts.force = true,
            "--skip-up" => opts.skip_up = true,
            "--skip-format" => opts.skip_format = true,
            "--skip-connect" => opts.skip_connect = true,
            "--skip-ssh-user" => opts.skip_ssh_user = true,
            "--replace-existing" => opts.replace_existing = true,
            "--bucket" => opts.bucket = take_value(args, "--bucket")?,
            "--volume" => opts.volume = take_value(args, "--volume")?,
            "--postgres-user" => opts.postgres_user = take_value(args, "--postgres-user")?,
            "--postgres-db" => opts.postgres_db = take_value(args, "--postgres-db")?,
            "--pairing-binary" => {
                let value = take_value(args, "--pairing-binary")?;
                opts.pairing_binary = Some(PathBuf::from(value));
            }
            "--wait-seconds" => {
                let value = take_value(args, "--wait-seconds")?;
                opts.wait_seconds = value
                    .parse::<u64>()
                    .map_err(|_| err("--wait-seconds must be an integer"))?;
            }
            _ => return Err(err(format!("unknown install flag: {}", arg))),
        }
    }

    Ok(opts)
}

pub fn run(opts: InstallOptions) -> Result<()> {
    if opts.skip_up && !opts.skip_format {
        return Err(err(
            "--skip-up requires --skip-format (services must be running to format)",
        ));
    }

    prepare_dirs(&opts.root)?;
    if !opts.skip_ssh_user {
        ensure_device_authorized_keys()?;
    }
    write_compose(&opts.root, opts.force, opts.replace_existing)?;
    warn_if_compose_outdated(&opts.root, opts.force)?;

    let env_config = ensure_env(&opts.root, &opts)?;
    ensure_s3_config(&opts.root, &env_config, opts.force)?;
    ensure_pairing_binary(&opts.root, opts.pairing_binary.as_deref())?;

    if !opts.skip_up {
        run_docker_compose(&opts.root)?;
    }

    if !opts.skip_format {
        format_juicefs(&opts.root, &env_config, opts.wait_seconds)?;
    }

    if !opts.skip_connect {
        connect_and_mount(&opts)?;
    }

    print_install_hints(&opts.root, opts.skip_connect)?;
    Ok(())
}

fn ensure_device_authorized_keys() -> Result<()> {
    #[cfg(unix)]
    {
        let home_dir = PathBuf::from("/home/hivec");
        let ssh_dir = home_dir.join(".ssh");
        let keys_path = ssh_dir.join("authorized_keys");

        if !running_as_root() {
            if keys_path.exists() {
                return Ok(());
            }
            eprintln!(
                "warning: {} missing; create it with provisioning or run as root",
                keys_path.display()
            );
            return Ok(());
        }

        let user = ensure_user("hivec", no_login_shell())?;
        fs::create_dir_all(&ssh_dir)
            .map_err(|e| err(format!("failed to create {}: {}", ssh_dir.display(), e)))?;
        if !keys_path.exists() {
            fs::write(&keys_path, "")
                .map_err(|e| err(format!("failed to write {}: {}", keys_path.display(), e)))?;
        }

        set_permissions(&home_dir, 0o755)?;
        set_permissions(&ssh_dir, 0o700)?;
        set_permissions(&keys_path, 0o600)?;
        chown_path(&home_dir, user.uid, user.gid)?;
        chown_path(&ssh_dir, user.uid, user.gid)?;
        chown_path(&keys_path, user.uid, user.gid)?;
    }
    Ok(())
}

#[cfg(unix)]
struct UserRecord {
    uid: u32,
    gid: u32,
}

#[cfg(unix)]
fn ensure_user(name: &str, shell: &str) -> Result<UserRecord> {
    if let Some(user) = lookup_user(name)? {
        return Ok(user);
    }

    let status = Command::new("useradd")
        .arg("-m")
        .arg("-s")
        .arg(shell)
        .arg(name)
        .status();
    let needs_fallback = match status {
        Ok(status) => !status.success(),
        Err(_) => true,
    };
    if needs_fallback {
        let status = Command::new("adduser")
            .arg("--disabled-password")
            .arg("--gecos")
            .arg("")
            .arg("--shell")
            .arg(shell)
            .arg(name)
            .status()
            .map_err(|e| err(format!("failed to run adduser: {}", e)))?;
        if !status.success() {
            return Err(err(format!(
                "failed to create user {} (useradd/adduser failed)",
                name
            )));
        }
    }

    lookup_user(name)?.ok_or_else(|| err(format!("failed to load user {} after create", name)))
}

#[cfg(unix)]
fn no_login_shell() -> &'static str {
    if Path::new("/usr/sbin/nologin").exists() {
        "/usr/sbin/nologin"
    } else {
        "/bin/false"
    }
}

#[cfg(unix)]
fn lookup_user(name: &str) -> Result<Option<UserRecord>> {
    let contents = fs::read_to_string("/etc/passwd")
        .map_err(|e| err(format!("failed to read /etc/passwd: {}", e)))?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split(':');
        let user = parts.next().unwrap_or("");
        if user != name {
            continue;
        }
        let _password = parts.next();
        let uid_str = parts.next().unwrap_or("");
        let gid_str = parts.next().unwrap_or("");
        let uid = uid_str
            .parse::<u32>()
            .map_err(|_| err("failed to parse uid"))?;
        let gid = gid_str
            .parse::<u32>()
            .map_err(|_| err("failed to parse gid"))?;
        return Ok(Some(UserRecord { uid, gid }));
    }
    Ok(None)
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

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let spec = format!("{}:{}", uid, gid);
    let status = Command::new("chown")
        .arg(spec)
        .arg(path)
        .status()
        .map_err(|e| err(format!("failed to run chown: {}", e)))?;
    if !status.success() {
        return Err(err(format!("failed to chown {}", path.display())));
    }
    Ok(())
}

#[cfg(unix)]
fn chown_path_recursive(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let spec = format!("{}:{}", uid, gid);
    let mut cmd = if running_as_root() {
        Command::new("chown")
    } else if can_sudo() {
        let mut cmd = Command::new("sudo");
        cmd.arg("-n").arg("chown");
        cmd
    } else {
        return Err(err(format!(
            "need root or passwordless sudo to chown {}",
            path.display()
        )));
    };

    let status = cmd
        .arg("-R")
        .arg(spec)
        .arg(path)
        .status()
        .map_err(|e| err(format!("failed to run chown: {}", e)))?;
    if !status.success() {
        return Err(err(format!("failed to chown -R {}", path.display())));
    }
    Ok(())
}

fn ensure_postgres_data_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        // Postgres in the container runs as uid/gid 70 (postgres).
        set_permissions(path, 0o700)?;
        chown_path_recursive(path, 70, 70)?;
    }
    Ok(())
}

fn can_sudo() -> bool {
    let status = Command::new("sudo")
        .arg("-n")
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(status) if status.success())
}

fn prepare_dirs(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(|e| err(format!("failed to create {}: {}", root.display(), e)))?;

    let data_dir = root.join("data");
    fs::create_dir_all(root.join("bin"))
        .map_err(|e| err(format!("failed to create bin dir: {}", e)))?;
    let postgres_dir = data_dir.join("postgres");
    fs::create_dir_all(&postgres_dir)
        .map_err(|e| err(format!("failed to create postgres data dir: {}", e)))?;
    ensure_postgres_data_dir(&postgres_dir)?;
    fs::create_dir_all(data_dir.join("seaweed"))
        .map_err(|e| err(format!("failed to create seaweed data dir: {}", e)))?;
    fs::create_dir_all(root.join("state"))
        .map_err(|e| err(format!("failed to create state dir: {}", e)))?;
    fs::create_dir_all(root.join("state").join("seaweedfs"))
        .map_err(|e| err(format!("failed to create seaweed state dir: {}", e)))?;
    fs::create_dir_all(root.join("state").join("pairing"))
        .map_err(|e| err(format!("failed to create pairing state dir: {}", e)))?;
    Ok(())
}

fn write_compose(root: &Path, force: bool, replace_existing: bool) -> Result<()> {
    let compose_path = root.join("compose.yml");
    if compose_path.exists() && !force {
        if replace_existing {
            return Err(err(
                "compose.yml already exists; rerun with --force to enable --replace-existing",
            ));
        }
        return Ok(());
    }

    let mut content = COMPOSE_TEMPLATE.to_string();
    if replace_existing && !content.contains("--replace-existing") {
        let needle = "        \"--authorized-keys\",\n";
        let replacement = "        \"--replace-existing\",\n        \"--authorized-keys\",\n";
        if content.contains(needle) {
            content = content.replacen(needle, replacement, 1);
        } else {
            return Err(err(
                "compose template missing pairing --authorized-keys entry",
            ));
        }
    }

    fs::write(&compose_path, content)
        .map_err(|e| err(format!("failed to write {}: {}", compose_path.display(), e)))?;
    Ok(())
}

fn warn_if_compose_outdated(root: &Path, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }

    let compose_path = root.join("compose.yml");
    let content = match fs::read_to_string(&compose_path) {
        Ok(content) => content,
        Err(_) => return Ok(()),
    };

    let has_seaweed_config = content.contains("-s3.config=/etc/seaweedfs/s3.json")
        && content.contains("./state/seaweedfs:/etc/seaweedfs");
    let has_pairing = content.contains("hive-core-pairing")
        && content.contains("pairing:");

    if has_seaweed_config && has_pairing {
        return Ok(());
    }

    eprintln!(
        "warning: compose.yml may be outdated; rerun with --force to update services"
    );
    Ok(())
}

fn ensure_env(root: &Path, opts: &InstallOptions) -> Result<EnvConfig> {
    let env_path = root.join(".env");
    let mut existing = if env_path.exists() {
        read_env_file(&env_path)?
    } else {
        HashMap::new()
    };

    let mut missing = Vec::new();

    let postgres_user = ensure_value(&mut existing, &mut missing, "POSTGRES_USER", opts.postgres_user.clone());
    let postgres_db = ensure_value(&mut existing, &mut missing, "POSTGRES_DB", opts.postgres_db.clone());

    let postgres_password = ensure_secret(&mut existing, &mut missing, "POSTGRES_PASSWORD", 24)?;
    let s3_access_key = ensure_secret(&mut existing, &mut missing, "S3_ACCESS_KEY", 16)?;
    let s3_secret_key = ensure_secret(&mut existing, &mut missing, "S3_SECRET_KEY", 24)?;

    let bucket = ensure_value(&mut existing, &mut missing, "HIVE_BUCKET", opts.bucket.clone());
    let volume = ensure_value(&mut existing, &mut missing, "HIVE_VOLUME", opts.volume.clone());

    if !env_path.exists() {
        let mut content = String::from("# Generated by hive-core install\n");
        for (key, value) in &missing {
            content.push_str(&format!("{}={}\n", key, value));
        }
        fs::write(&env_path, content)
            .map_err(|e| err(format!("failed to write {}: {}", env_path.display(), e)))?;
    } else if !missing.is_empty() {
        append_env_values(&env_path, &missing)?;
    }

    set_env_permissions(&env_path)?;

    Ok(EnvConfig {
        postgres_user,
        postgres_db,
        postgres_password,
        s3_access_key,
        s3_secret_key,
        bucket,
        volume,
    })
}

fn ensure_value(
    existing: &mut HashMap<String, String>,
    missing: &mut Vec<(String, String)>,
    key: &str,
    default_value: String,
) -> String {
    match existing.get(key) {
        Some(value) if !value.is_empty() => value.clone(),
        _ => {
            existing.insert(key.to_string(), default_value.clone());
            missing.push((key.to_string(), default_value.clone()));
            default_value
        }
    }
}

fn ensure_secret(
    existing: &mut HashMap<String, String>,
    missing: &mut Vec<(String, String)>,
    key: &str,
    bytes: usize,
) -> Result<String> {
    if let Some(value) = existing.get(key) {
        if !value.is_empty() {
            return Ok(value.clone());
        }
    }

    let secret = generate_hex_secret(bytes)?;
    existing.insert(key.to_string(), secret.clone());
    missing.push((key.to_string(), secret.clone()));
    Ok(secret)
}

fn read_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let contents = fs::read_to_string(path)
        .map_err(|e| err(format!("failed to read {}: {}", path.display(), e)))?;
    let mut map = HashMap::new();
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

fn append_env_values(path: &Path, values: &[(String, String)]) -> Result<()> {
    let needs_newline = match fs::read(path) {
        Ok(bytes) => bytes.last().map(|b| *b != b'\n').unwrap_or(false),
        Err(_) => false,
    };

    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| err(format!("failed to open {}: {}", path.display(), e)))?;

    if needs_newline {
        file.write_all(b"\n")
            .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    }

    for (key, value) in values {
        writeln!(file, "{}={}", key, value)
            .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    }

    Ok(())
}

fn set_env_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .map_err(|e| err(format!("failed to set permissions on {}: {}", path.display(), e)))?;
    }
    Ok(())
}

fn ensure_s3_config(root: &Path, env: &EnvConfig, force: bool) -> Result<()> {
    let config_path = root.join("state").join("seaweedfs").join("s3.json");
    if config_path.exists() && !force {
        set_s3_config_permissions(&config_path)?;
        return Ok(());
    }

    let config = json!({
        "identities": [
            {
                "name": "hive",
                "credentials": [
                    {
                        "accessKey": env.s3_access_key,
                        "secretKey": env.s3_secret_key
                    }
                ],
                "actions": ["Admin", "Read", "Write", "List", "Tagging"]
            }
        ]
    });

    let content = serde_json::to_string_pretty(&config)
        .map_err(|e| err(format!("failed to serialize s3 config: {}", e)))?;
    fs::write(&config_path, content)
        .map_err(|e| err(format!("failed to write {}: {}", config_path.display(), e)))?;
    set_s3_config_permissions(&config_path)?;
    Ok(())
}

fn set_s3_config_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(path, perms)
            .map_err(|e| err(format!("failed to set permissions on {}: {}", path.display(), e)))?;
    }
    Ok(())
}

fn generate_hex_secret(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    let mut file = File::open("/dev/urandom")
        .map_err(|e| err(format!("failed to open /dev/urandom: {}", e)))?;
    file.read_exact(&mut buffer)
        .map_err(|e| err(format!("failed to read /dev/urandom: {}", e)))?;
    let mut output = String::with_capacity(bytes * 2);
    for byte in buffer {
        output.push_str(&format!("{:02x}", byte));
    }
    Ok(output)
}

fn run_docker_compose(root: &Path) -> Result<()> {
    let status = Command::new("docker")
        .arg("compose")
        .arg("up")
        .arg("-d")
        .current_dir(root)
        .status()
        .map_err(|e| err(format!("failed to run docker compose: {}", e)))?;

    if !status.success() {
        return Err(err(format!(
            "docker compose failed with status {:?}",
            status.code()
        )));
    }
    Ok(())
}

fn ensure_pairing_binary(root: &Path, pairing_binary: Option<&Path>) -> Result<()> {
    let binary_dst = root.join("bin").join("hive-core-pairing");
    if let Some(path) = pairing_binary {
        if !path.exists() {
            return Err(err(format!(
                "pairing binary missing: {}",
                path.display()
            )));
        }
        fs::copy(path, &binary_dst)
            .map_err(|e| err(format!("failed to copy pairing binary: {}", e)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&binary_dst, perms).ok();
        }
        return Ok(());
    }

    if binary_dst.exists() {
        return Ok(());
    }

    let repo_root = repo_root();
    let cargo_toml = repo_root.join("Cargo.toml");
    if !cargo_toml.exists() {
        return Err(err(format!(
            "pairing build requires source checkout (missing {})",
            cargo_toml.display()
        )));
    }

    let status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("hive-core-pairing")
        .arg("--release")
        .current_dir(&repo_root)
        .status()
        .map_err(|e| err(format!("failed to build pairing server: {}", e)))?;
    if !status.success() {
        return Err(err("pairing server build failed"));
    }

    let binary_src = repo_root.join("target").join("release").join("hive-core-pairing");
    if !binary_src.exists() {
        return Err(err(format!(
            "pairing binary missing after build: {}",
            binary_src.display()
        )));
    }
    fs::copy(&binary_src, &binary_dst)
        .map_err(|e| err(format!("failed to copy pairing binary: {}", e)))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&binary_dst, perms).ok();
    }

    Ok(())
}

fn connect_and_mount(opts: &InstallOptions) -> Result<()> {
    let mountpoint = PathBuf::from("/home/hive/hive");
    ensure_mountpoint(&mountpoint)?;

    let envelope_path = temp_envelope_path();
    let connect_opts = connect::ConnectOptions {
        root: opts.root.clone(),
        host: Some("127.0.0.1".to_string()),
        port: 22,
        user: "hivec".to_string(),
        display_name: "Hive".to_string(),
        out: Some(envelope_path.clone()),
        pretty: false,
        fingerprint: None,
    };
    connect::run(connect_opts)?;
    set_permissions(&envelope_path, 0o644)?;

    let desktop_cli = ensure_desktop_cli_binary()?;
    spawn_desktop_connect(
        &desktop_cli,
        &envelope_path,
        &mountpoint,
        opts.replace_existing,
        &opts.root,
    )?;

    let _ = fs::remove_file(&envelope_path);
    Ok(())
}

fn ensure_desktop_cli_binary() -> Result<PathBuf> {
    let repo_root = repo_root();
    let cargo_toml = repo_root.join("Cargo.toml");
    if !cargo_toml.exists() {
        return Err(err(format!(
            "desktop cli build requires source checkout (missing {})",
            cargo_toml.display()
        )));
    }

    let status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("hive-desktop-cli")
        .arg("--release")
        .current_dir(&repo_root)
        .status()
        .map_err(|e| err(format!("failed to build hive-desktop-cli: {}", e)))?;
    if !status.success() {
        return Err(err("hive-desktop-cli build failed"));
    }

    let binary = repo_root.join("target").join("release").join("hive-desktop-cli");
    if !binary.exists() {
        return Err(err(format!(
            "hive-desktop-cli binary missing after build: {}",
            binary.display()
        )));
    }
    Ok(binary)
}

fn spawn_desktop_connect(
    binary: &Path,
    envelope_path: &Path,
    mountpoint: &Path,
    replace_existing: bool,
    root: &Path,
) -> Result<()> {
    let log_path = root.join("state").join("host-mount.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| err(format!("failed to open {}: {}", log_path.display(), e)))?;
    let mut cmd = desktop_cli_command(binary)?;
    cmd.arg("connect")
        .arg("--envelope")
        .arg(envelope_path)
        .arg("--mountpoint")
        .arg(mountpoint)
        .arg("--accept-host-key")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log_file
                .try_clone()
                .map_err(|e| err(format!("failed to clone log file: {}", e)))?,
        ))
        .stderr(Stdio::from(log_file));

    if replace_existing {
        cmd.arg("--replace-existing");
    }

    detach_child_process(&mut cmd)?;

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound && running_as_root() {
            err("sudo not found; install it or rerun with --skip-connect")
        } else {
            err(format!("failed to start local mount: {}", e))
        }
    })?;

    std::thread::sleep(Duration::from_secs(1));
    if let Some(status) = child
        .try_wait()
        .map_err(|e| err(format!("failed to check mount status: {}", e)))?
    {
        return Err(err(format!(
            "local mount failed to start (exit {:?}); see {}",
            status.code(),
            log_path.display()
        )));
    }

    println!(
        "local mount started at {} (pid {}, logs: {})",
        mountpoint.display(),
        child.id(),
        log_path.display()
    );
    Ok(())
}

fn detach_child_process(cmd: &mut Command) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    Ok(())
}

fn desktop_cli_command(binary: &Path) -> Result<Command> {
    if running_as_root() {
        return Ok(sudo_as_hive(binary, false));
    }

    if can_sudo_as_hive() {
        return Ok(sudo_as_hive(binary, true));
    }

    Ok(Command::new(binary))
}

fn sudo_as_hive(binary: &Path, non_interactive: bool) -> Command {
    let mut cmd = Command::new("sudo");
    if non_interactive {
        cmd.arg("-n");
    }
    cmd.arg("-H").arg("-u").arg("hive").arg(binary);
    cmd
}

fn can_sudo_as_hive() -> bool {
    let status = Command::new("sudo")
        .arg("-n")
        .arg("-u")
        .arg("hive")
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(status) if status.success())
}

fn running_as_root() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("Uid:") {
                    let uid = rest.split_whitespace().next().unwrap_or("");
                    return uid == "0";
                }
            }
        }
    }

    std::env::var("USER")
        .map(|user| user == "root")
        .unwrap_or(false)
}

fn ensure_mountpoint(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| err(format!("failed to create {}: {}", path.display(), e)))?;
    #[cfg(unix)]
    {
        let user = ensure_user("hive", "/bin/bash")?;
        set_permissions(path, 0o755)?;
        chown_path(path, user.uid, user.gid)?;
    }
    Ok(())
}

fn temp_envelope_path() -> PathBuf {
    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("hive-core-envelope-{pid}-{now}.json"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn format_juicefs(root: &Path, env: &EnvConfig, wait_seconds: u64) -> Result<()> {
    let marker_path = root.join("state").join("juicefs.format");
    if marker_path.exists() {
        return Ok(());
    }

    wait_for_postgres_ready(root, env, wait_seconds)?;
    wait_for_s3_ready("127.0.0.1:8333", wait_seconds)?;

    let bucket_url = format!("http://127.0.0.1:8333/{}", env.bucket);
    let meta_url = format!(
        "postgres://{}@127.0.0.1:5432/{}",
        env.postgres_user, env.postgres_db
    );

    let status = Command::new("juicefs")
        .arg("format")
        .arg("--storage")
        .arg("s3")
        .arg("--bucket")
        .arg(bucket_url)
        .arg(meta_url)
        .arg(&env.volume)
        .env("META_PASSWORD", &env.postgres_password)
        .env("ACCESS_KEY", &env.s3_access_key)
        .env("SECRET_KEY", &env.s3_secret_key)
        .status()
        .map_err(|e| err(format!("failed to run juicefs format: {}", e)))?;

    if !status.success() {
        return Err(err(format!("juicefs format failed with status {:?}", status.code())));
    }

    write_format_marker(&marker_path, env)?;
    Ok(())
}

fn wait_for_postgres_ready(root: &Path, env: &EnvConfig, wait_seconds: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        if is_postgres_ready(root, env) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(err(
                "timed out waiting for postgres to be ready (check docker logs)",
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn is_postgres_ready(root: &Path, env: &EnvConfig) -> bool {
    let status = Command::new("docker")
        .arg("compose")
        .arg("exec")
        .arg("-T")
        .arg("postgres")
        .arg("pg_isready")
        .arg("-U")
        .arg(&env.postgres_user)
        .arg("-d")
        .arg(&env.postgres_db)
        .current_dir(root)
        .status();

    matches!(status, Ok(status) if status.success())
}

fn wait_for_s3_ready(addr: &str, wait_seconds: u64) -> Result<()> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|_| err(format!("invalid address: {}", addr)))?;
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    loop {
        if check_http(&socket) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(err(
                "timed out waiting for seaweedfs s3 to respond (check docker logs)",
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn check_http(addr: &SocketAddr) -> bool {
    let mut stream = match TcpStream::connect_timeout(addr, Duration::from_secs(1)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };

    if stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return false;
    }

    let mut buf = [0u8; 32];
    match stream.read(&mut buf) {
        Ok(0) => false,
        Ok(_) => true,
        Err(_) => false,
    }
}

fn write_format_marker(path: &Path, env: &EnvConfig) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| err(format!("failed to read system time: {}", e)))?
        .as_secs();
    let content = format!(
        "volume={}\nbucket={}\nformatted_at={}\n",
        env.volume, env.bucket, timestamp
    );
    fs::write(path, content)
        .map_err(|e| err(format!("failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn print_install_hints(root: &Path, skip_connect: bool) -> Result<()> {
    println!("install complete");
    println!("root: {}", root.display());
    println!("next steps:");
    if skip_connect {
        println!("- local mount skipped (--skip-connect)");
    } else {
        println!(
            "- local mount: /home/hive/hive (logs at {})",
            root.join("state").join("host-mount.log").display()
        );
        println!("- rerun with --skip-connect to disable local mount");
    }
    println!("- export a pairing envelope: hive-core connect --host <public-host> --user hivec");
    println!("- share the SSH host key fingerprint from: hive-core fingerprint");
    println!("- configure Caddy to proxy https://<host>/hive-pair to http://127.0.0.1:8081");
    println!("- for localhost dev, use http://localhost:8081/hive-pair");
    Ok(())
}
