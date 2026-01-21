use crate::util::{err, take_value, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
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
    pub bucket: String,
    pub volume: String,
    pub postgres_user: String,
    pub postgres_db: String,
    pub wait_seconds: u64,
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
        bucket: "hive".to_string(),
        volume: "hive".to_string(),
        postgres_user: "juicefs".to_string(),
        postgres_db: "juicefs_meta".to_string(),
        wait_seconds: 60,
    };

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--root" => opts.root = PathBuf::from(take_value(args, "--root")?),
            "--force" => opts.force = true,
            "--skip-up" => opts.skip_up = true,
            "--skip-format" => opts.skip_format = true,
            "--bucket" => opts.bucket = take_value(args, "--bucket")?,
            "--volume" => opts.volume = take_value(args, "--volume")?,
            "--postgres-user" => opts.postgres_user = take_value(args, "--postgres-user")?,
            "--postgres-db" => opts.postgres_db = take_value(args, "--postgres-db")?,
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
    write_compose(&opts.root, opts.force)?;
    warn_if_compose_outdated(&opts.root, opts.force)?;

    let env_config = ensure_env(&opts.root, &opts)?;
    ensure_s3_config(&opts.root, &env_config, opts.force)?;

    if !opts.skip_up {
        run_docker_compose(&opts.root)?;
    }

    if !opts.skip_format {
        format_juicefs(&opts.root, &env_config, opts.wait_seconds)?;
    }

    print_install_hints(&opts.root)?;
    Ok(())
}

fn prepare_dirs(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(|e| err(format!("failed to create {}: {}", root.display(), e)))?;

    let data_dir = root.join("data");
    fs::create_dir_all(data_dir.join("postgres"))
        .map_err(|e| err(format!("failed to create postgres data dir: {}", e)))?;
    fs::create_dir_all(data_dir.join("seaweed"))
        .map_err(|e| err(format!("failed to create seaweed data dir: {}", e)))?;
    fs::create_dir_all(root.join("state"))
        .map_err(|e| err(format!("failed to create state dir: {}", e)))?;
    fs::create_dir_all(root.join("state").join("seaweedfs"))
        .map_err(|e| err(format!("failed to create seaweed state dir: {}", e)))?;
    Ok(())
}

fn write_compose(root: &Path, force: bool) -> Result<()> {
    let compose_path = root.join("compose.yml");
    if compose_path.exists() && !force {
        return Ok(());
    }

    fs::write(&compose_path, COMPOSE_TEMPLATE)
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

    if content.contains("-s3.config=/etc/seaweedfs/s3.json")
        && content.contains("./state/seaweedfs:/etc/seaweedfs")
    {
        return Ok(());
    }

    eprintln!(
        "warning: compose.yml may be outdated; rerun with --force to update seaweedfs s3 config"
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
    set_secret_permissions(&config_path)?;
    Ok(())
}

fn set_secret_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
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

fn print_install_hints(root: &Path) -> Result<()> {
    println!("install complete");
    println!("root: {}", root.display());
    println!("next steps:");
    println!("- add a device key with: hive-core device add --name <NAME> --pubkey-file <PATH>");
    println!("- share the SSH host key fingerprint from: hive-core fingerprint");
    Ok(())
}
