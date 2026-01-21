use std::collections::BTreeMap;
use std::net::TcpListener;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use hive_protocol::{Profile, TunnelConfig};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::errors::{DesktopError, Result};

#[derive(Debug)]
pub struct TempKeyFile {
    path: std::path::PathBuf,
}

impl TempKeyFile {
    pub fn new(key_material: &str) -> Result<Self> {
        // TODO: Avoid writing SSH keys to disk by integrating with ssh-agent or OS keychain APIs.
        let (path, mut file) = create_temp_file()?;
        #[cfg(not(unix))]
        {
            // TODO: Use explicit ACLs to lock down the temp file on Windows.
        }
        use std::io::Write;
        file.write_all(key_material.as_bytes())?;
        #[cfg(not(unix))]
        {
            let mut perms = file.metadata()?.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempKeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedTunnel {
    pub name: String,
    pub local_bind_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

#[derive(Debug)]
pub struct TunnelSession {
    pub tunnels: Vec<ResolvedTunnel>,
    pub local_ports: BTreeMap<String, u16>,
    _identity_key: TempKeyFile,
    child: Child,
}

impl TunnelSession {
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub async fn is_healthy(&mut self, timeout: Duration) -> Result<bool> {
        if let Some(_) = self.child.try_wait()? {
            return Ok(false);
        }
        for tunnel in &self.tunnels {
            if !check_port(&tunnel.local_bind_host, tunnel.local_port, timeout).await {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn wait_healthy(&mut self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.is_healthy(timeout).await? {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(DesktopError::Timeout("waiting for tunnel"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.child.kill();
        let _ = self.child.wait().await;
        Ok(())
    }
}

pub async fn start_tunnel(
    profile: &Profile,
    identity_key: TempKeyFile,
    ssh_path: &Path,
    known_hosts_path: &Path,
) -> Result<TunnelSession> {
    ensure_known_hosts_file(known_hosts_path)?;
    let tunnels = resolve_tunnels(&profile.tunnels)?;
    if tunnels.is_empty() {
        return Err(DesktopError::InvalidProfile("no tunnels defined".to_string()));
    }

    let mut cmd = Command::new(ssh_path);
    cmd.arg("-N").arg("-T");
    cmd.arg("-o").arg("ExitOnForwardFailure=yes");
    cmd.arg("-o").arg("ServerAliveInterval=10");
    cmd.arg("-o").arg("ServerAliveCountMax=3");
    cmd.arg("-o").arg("BatchMode=yes");
    cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
    cmd.arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts_path.display()));
    cmd.arg("-o").arg("IdentitiesOnly=yes");
    cmd.arg("-i").arg(identity_key.path());
    cmd.arg("-p").arg(profile.ssh.port.to_string());

    for tunnel in &tunnels {
        let spec = format!(
            "{}:{}:{}:{}",
            tunnel.local_bind_host, tunnel.local_port, tunnel.remote_host, tunnel.remote_port
        );
        cmd.arg("-L").arg(spec);
    }

    cmd.arg(format!("{}@{}", profile.ssh.user, profile.ssh.host));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            DesktopError::Ssh(format!("ssh binary not found at {}", ssh_path.display()))
        } else {
            err.into()
        }
    })?;

    let local_ports = tunnels
        .iter()
        .map(|tunnel| (tunnel.name.clone(), tunnel.local_port))
        .collect();

    Ok(TunnelSession {
        tunnels,
        local_ports,
        _identity_key: identity_key,
        child,
    })
}

fn ensure_known_hosts_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(())
}

fn resolve_tunnels(configs: &[TunnelConfig]) -> Result<Vec<ResolvedTunnel>> {
    let mut resolved = Vec::with_capacity(configs.len());
    for config in configs {
        let local_port = if config.local_port == 0 {
            allocate_port(&config.local_bind_host)?
        } else {
            config.local_port
        };
        resolved.push(ResolvedTunnel {
            name: config.name.clone(),
            local_bind_host: config.local_bind_host.clone(),
            local_port,
            remote_host: config.remote_host.clone(),
            remote_port: config.remote_port,
        });
    }
    Ok(resolved)
}

fn allocate_port(bind_host: &str) -> Result<u16> {
    let bind_addr = format!("{}:0", bind_host);
    let listener = TcpListener::bind(bind_addr)?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

fn create_temp_file() -> Result<(std::path::PathBuf, std::fs::File)> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    for attempt in 0..100 {
        let filename = format!("hive-agents-ssh-key-{pid}-{now}-{attempt}.tmp");
        let path = base.join(filename);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(DesktopError::Ssh(
        "failed to create temp key file".to_string(),
    ))
}

async fn check_port(host: &str, port: u16, timeout: Duration) -> bool {
    let addr = format!("{}:{}", host, port);
    match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
        Ok(Ok(_)) => true,
        _ => false,
    }
}
