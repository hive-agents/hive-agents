use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use hive_protocol::{EndpointMode, LocalSettings, Profile, TunnelConfig};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::errors::{DesktopError, Result};
use crate::health::{run_preflight, PostgresPreflight, S3Preflight};
use crate::mount::{start_mount, MountEnv, MountSession};
use crate::tunnel::{start_tunnel, TempKeyFile, TunnelSession};
use crate::SecretStore;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorState {
    Idle,
    Connecting,
    TunnelStarting,
    TunnelHealthy,
    PreflightChecks,
    MountStarting,
    Mounted,
    Degraded,
    Reconnecting,
    Disconnecting,
    Unmounting,
    TunnelStopping,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    Supervisor,
    Tunnel,
    Mount,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub source: LogSource,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: SupervisorState,
    pub last_error: Option<String>,
    pub local_ports: BTreeMap<String, u16>,
    pub mountpoint: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub ssh_path: PathBuf,
    pub juicefs_path: PathBuf,
    pub known_hosts_path: PathBuf,
    pub health_interval: Duration,
    pub mount_check_interval: Duration,
    pub mount_timeout: Duration,
    pub tunnel_timeout: Duration,
    pub port_check_timeout: Duration,
    pub preflight_timeout: Duration,
    pub log_capacity: usize,
    pub postgres_tunnel_name: String,
    pub s3_tunnel_name: String,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            ssh_path: PathBuf::from("ssh"),
            juicefs_path: PathBuf::from("juicefs"),
            known_hosts_path: PathBuf::from("known_hosts"),
            health_interval: Duration::from_secs(5),
            mount_check_interval: Duration::from_millis(500),
            mount_timeout: Duration::from_secs(60),
            tunnel_timeout: Duration::from_secs(10),
            port_check_timeout: Duration::from_secs(1),
            preflight_timeout: Duration::from_secs(5),
            log_capacity: 200,
            postgres_tunnel_name: "postgres".to_string(),
            s3_tunnel_name: "s3".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct SupervisorHandle {
    command_tx: mpsc::Sender<Command>,
    status: Arc<Mutex<Status>>, 
    logs: Arc<Mutex<LogBuffer>>,
}

impl SupervisorHandle {
    pub fn spawn(
        profile: Profile,
        local_settings: LocalSettings,
        secrets: Arc<dyn SecretStore>,
        config: SupervisorConfig,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel(8);
        let status = Arc::new(Mutex::new(Status {
            state: SupervisorState::Idle,
            last_error: None,
            local_ports: BTreeMap::new(),
            mountpoint: None,
        }));
        let logs = Arc::new(Mutex::new(LogBuffer::new(config.log_capacity)));

        let supervisor = Supervisor {
            profile,
            local_settings,
            secrets,
            config,
            command_rx,
            state: SupervisorState::Idle,
            tunnel: None,
            mount: None,
            status: Arc::clone(&status),
            logs: Arc::clone(&logs),
            reconnect_attempts: 0,
            next_reconnect_at: None,
        };

        tokio::spawn(supervisor.run());

        Self {
            command_tx,
            status,
            logs,
        }
    }

    pub async fn connect(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.command_tx
            .send(Command::Connect { reply })
            .await
            .map_err(|_| DesktopError::Supervisor("connect channel closed".to_string()))?;
        rx.await
            .map_err(|_| DesktopError::Supervisor("connect response dropped".to_string()))?
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.disconnect_inner(false).await
    }

    pub async fn force_disconnect(&self) -> Result<()> {
        self.disconnect_inner(true).await
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or(Status {
                state: SupervisorState::Error,
                last_error: Some("status lock poisoned".to_string()),
                local_ports: BTreeMap::new(),
                mountpoint: None,
            })
    }

    pub fn logs_tail(&self, count: usize) -> Vec<LogLine> {
        self.logs
            .lock()
            .map(|buffer| buffer.tail(count))
            .unwrap_or_default()
    }

    async fn disconnect_inner(&self, force: bool) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.command_tx
            .send(Command::Disconnect { force, reply })
            .await
            .map_err(|_| DesktopError::Supervisor("disconnect channel closed".to_string()))?;
        rx.await
            .map_err(|_| DesktopError::Supervisor("disconnect response dropped".to_string()))?
    }
}

enum Command {
    Connect { reply: oneshot::Sender<Result<()>> },
    Disconnect { force: bool, reply: oneshot::Sender<Result<()>> },
}

struct Supervisor {
    profile: Profile,
    local_settings: LocalSettings,
    secrets: Arc<dyn SecretStore>,
    config: SupervisorConfig,
    command_rx: mpsc::Receiver<Command>,
    state: SupervisorState,
    tunnel: Option<TunnelSession>,
    mount: Option<MountSession>,
    status: Arc<Mutex<Status>>,
    logs: Arc<Mutex<LogBuffer>>,
    reconnect_attempts: u32,
    next_reconnect_at: Option<Instant>,
}

impl Supervisor {
    async fn run(mut self) {
        let mut health_interval = tokio::time::interval(self.config.health_interval);
        loop {
            tokio::select! {
                _ = health_interval.tick() => {
                    self.on_health_tick().await;
                }
                command = self.command_rx.recv() => {
                    match command {
                        Some(command) => {
                            self.handle_command(command).await;
                        }
                        None => break,
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Connect { reply } => {
                let result = self.connect_sequence().await;
                if let Err(err) = &result {
                    self.handle_failure(&err.to_string()).await;
                }
                let _ = reply.send(result);
            }
            Command::Disconnect { force, reply } => {
                let result = self.disconnect_sequence(force).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn connect_sequence(&mut self) -> Result<()> {
        if !matches!(self.state, SupervisorState::Idle) {
            return Err(DesktopError::Supervisor(
                "connect called while not idle".to_string(),
            ));
        }

        self.clear_last_error();
        self.set_state(SupervisorState::Connecting);
        self.log(LogLevel::Info, LogSource::Supervisor, "connecting");

        self.set_state(SupervisorState::TunnelStarting);
        let identity_key_material = self
            .secrets
            .resolve_text(&self.profile.ssh.identity_key_ref)?;
        let identity_key = TempKeyFile::new(&identity_key_material)?;
        let mut tunnel = start_tunnel(
            &self.profile,
            identity_key,
            &self.config.ssh_path,
            &self.config.known_hosts_path,
        )
        .await?;
        self.attach_child_logs(LogSource::Tunnel, tunnel.take_stdout(), tunnel.take_stderr());
        self.tunnel = Some(tunnel);
        let ports = {
            let tunnel = self
                .tunnel
                .as_mut()
                .ok_or_else(|| DesktopError::Supervisor("tunnel missing".to_string()))?;
            tunnel.wait_healthy(self.config.tunnel_timeout).await?;
            tunnel.local_ports.clone()
        };
        self.update_ports(ports);
        self.set_state(SupervisorState::TunnelHealthy);

        self.set_state(SupervisorState::PreflightChecks);
        let pg_password = self
            .secrets
            .resolve_text(&self.profile.juicefs.meta.password_ref)?;
        let access_key = self
            .secrets
            .resolve_text(&self.profile.juicefs.object.access_key_ref)?;
        let secret_key = self
            .secrets
            .resolve_text(&self.profile.juicefs.object.secret_key_ref)?;

        let postgres_tunnel = self.lookup_tunnel(&self.config.postgres_tunnel_name)?;
        let pg_port = self.lookup_local_port(&self.config.postgres_tunnel_name)?;
        let pg_preflight = PostgresPreflight {
            host: postgres_tunnel.local_bind_host.clone(),
            port: pg_port,
            user: self.profile.juicefs.meta.user.clone(),
            database: self.profile.juicefs.meta.database.clone(),
            password: pg_password.clone(),
        };

        let s3_port = if matches!(self.profile.juicefs.object.endpoint_mode, EndpointMode::Tunneled)
        {
            Some(self.lookup_local_port(&self.config.s3_tunnel_name)?)
        } else {
            None
        };

        let bucket_url = render_template(
            &self.profile.juicefs.object.bucket_url_template,
            Some(pg_port),
            s3_port,
        )?;
        let s3_preflight = S3Preflight {
            endpoint_url: bucket_url.clone(),
        };

        run_preflight(&pg_preflight, &s3_preflight, self.config.preflight_timeout).await?;

        self.set_state(SupervisorState::MountStarting);

        let meta_url = render_template(
            &self.profile.juicefs.meta.dsn_template,
            Some(pg_port),
            s3_port,
        )?;

        let mount_env = MountEnv {
            meta_password: pg_password,
            access_key,
            secret_key,
        };

        let mut mount = start_mount(
            &self.config.juicefs_path,
            &self.local_settings.mountpoint.path,
            &meta_url,
            &bucket_url,
            &self.local_settings.cache.cache_dir,
            self.local_settings.cache.cache_size_mib,
            self.profile.juicefs.object.storage,
            mount_env,
        )
        .await?;

        self.attach_child_logs(LogSource::Mount, mount.take_stdout(), mount.take_stderr());
        self.mount = Some(mount);
        if let Some(mount) = self.mount.as_mut() {
            mount
                .wait_mounted(self.config.mount_timeout, self.config.mount_check_interval)
                .await?;
        }
        self.set_state(SupervisorState::Mounted);
        if let Some(mount) = self.mount.as_ref() {
            self.update_mountpoint(Some(mount.mountpoint().to_path_buf()));
        }
        self.log(LogLevel::Info, LogSource::Supervisor, "mounted");

        Ok(())
    }

    async fn disconnect_sequence(&mut self, force: bool) -> Result<()> {
        if matches!(self.state, SupervisorState::Idle) {
            return Ok(());
        }

        self.set_state(SupervisorState::Disconnecting);

        if let Some(mut mount) = self.mount.take() {
            self.set_state(SupervisorState::Unmounting);
            if let Err(err) = mount.unmount(force).await {
                self.log(
                    LogLevel::Warn,
                    LogSource::Supervisor,
                    &format!("unmount failed: {}", err),
                );
                let _ = mount.stop().await;
            }
        }

        if let Some(mut tunnel) = self.tunnel.take() {
            self.set_state(SupervisorState::TunnelStopping);
            let _ = tunnel.stop().await;
        }

        self.update_ports(BTreeMap::new());
        self.update_mountpoint(None);
        self.set_state(SupervisorState::Idle);
        self.clear_last_error();
        self.log(LogLevel::Info, LogSource::Supervisor, "disconnected");
        Ok(())
    }

    async fn on_health_tick(&mut self) {
        if let Some(tunnel) = self.tunnel.as_mut() {
            if let Ok(Some(status)) = tunnel.try_wait() {
                self.log(
                    LogLevel::Error,
                    LogSource::Tunnel,
                    &format!("tunnel exited: {status}"),
                );
                self.handle_failure("tunnel exited").await;
                return;
            }
        }

        if let Some(mount) = self.mount.as_mut() {
            if let Ok(Some(status)) = mount.try_wait() {
                self.log(
                    LogLevel::Error,
                    LogSource::Mount,
                    &format!("mount exited: {status}"),
                );
                self.handle_failure("mount exited").await;
                return;
            }
        }

        match self.state {
            SupervisorState::Mounted => {
                if let Some(tunnel) = self.tunnel.as_mut() {
                    match tunnel.is_healthy(self.config.port_check_timeout).await {
                        Ok(true) => {}
                        Ok(false) => {
                            self.set_state(SupervisorState::Degraded);
                            self.log(
                                LogLevel::Warn,
                                LogSource::Supervisor,
                                "tunnel unhealthy",
                            );
                            self.schedule_reconnect();
                        }
                        Err(err) => {
                            self.log(
                                LogLevel::Warn,
                                LogSource::Supervisor,
                                &format!("tunnel health check failed: {}", err),
                            );
                        }
                    }
                }
            }
            SupervisorState::Degraded => {
                if self.should_reconnect() {
                    self.set_state(SupervisorState::Reconnecting);
                    if let Err(err) = self.reconnect_tunnel().await {
                        self.log(
                            LogLevel::Warn,
                            LogSource::Supervisor,
                            &format!("reconnect failed: {}", err),
                        );
                        self.schedule_reconnect();
                        self.set_state(SupervisorState::Degraded);
                    } else {
                        self.reconnect_attempts = 0;
                        self.next_reconnect_at = None;
                        self.set_state(SupervisorState::Mounted);
                    }
                }
            }
            _ => {}
        }
    }

    async fn reconnect_tunnel(&mut self) -> Result<()> {
        if let Some(mut tunnel) = self.tunnel.take() {
            let _ = tunnel.stop().await;
        }

        let identity_key_material = self
            .secrets
            .resolve_text(&self.profile.ssh.identity_key_ref)?;
        let identity_key = TempKeyFile::new(&identity_key_material)?;
        let mut tunnel = start_tunnel(
            &self.profile,
            identity_key,
            &self.config.ssh_path,
            &self.config.known_hosts_path,
        )
        .await?;
        self.attach_child_logs(LogSource::Tunnel, tunnel.take_stdout(), tunnel.take_stderr());
        tunnel.wait_healthy(self.config.tunnel_timeout).await?;
        self.update_ports(tunnel.local_ports.clone());
        self.tunnel = Some(tunnel);
        Ok(())
    }

    fn schedule_reconnect(&mut self) {
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        let delay = backoff_delay(self.reconnect_attempts);
        self.next_reconnect_at = Some(Instant::now() + delay);
    }

    fn should_reconnect(&self) -> bool {
        self.next_reconnect_at
            .map(|at| Instant::now() >= at)
            .unwrap_or(true)
    }

    async fn handle_failure(&mut self, message: &str) {
        self.log(LogLevel::Error, LogSource::Supervisor, message);
        let _ = self.disconnect_sequence(true).await;
        self.set_state(SupervisorState::Error);
        self.set_last_error(message.to_string());
    }

    fn lookup_tunnel(&self, name: &str) -> Result<&TunnelConfig> {
        self.profile
            .tunnels
            .iter()
            .find(|tunnel| tunnel.name == name)
            .ok_or_else(|| {
                DesktopError::InvalidProfile(format!("missing tunnel config: {name}"))
            })
    }

    fn lookup_local_port(&self, name: &str) -> Result<u16> {
        self.tunnel
            .as_ref()
            .and_then(|tunnel| tunnel.local_ports.get(name).copied())
            .ok_or_else(|| DesktopError::InvalidProfile(format!("missing tunnel port: {name}")))
    }

    fn update_ports(&self, ports: BTreeMap<String, u16>) {
        if let Ok(mut status) = self.status.lock() {
            status.local_ports = ports;
        }
    }

    fn update_mountpoint(&self, mountpoint: Option<PathBuf>) {
        if let Ok(mut status) = self.status.lock() {
            status.mountpoint = mountpoint;
        }
    }

    fn set_state(&mut self, state: SupervisorState) {
        self.state = state.clone();
        if let Ok(mut status) = self.status.lock() {
            status.state = state;
        }
    }

    fn set_last_error(&self, message: String) {
        if let Ok(mut status) = self.status.lock() {
            status.last_error = Some(message);
        }
    }

    fn clear_last_error(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_error = None;
        }
    }

    fn log(&self, level: LogLevel, source: LogSource, message: &str) {
        if let Ok(mut buffer) = self.logs.lock() {
            buffer.push(LogLine {
                timestamp: Utc::now(),
                level,
                source,
                message: message.to_string(),
            });
        }
    }

    fn attach_child_logs(
        &self,
        source: LogSource,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
    ) {
        if let Some(stdout) = stdout {
            spawn_log_task(source.clone(), LogLevel::Info, stdout, Arc::clone(&self.logs));
        }
        if let Some(stderr) = stderr {
            spawn_log_task(source, LogLevel::Warn, stderr, Arc::clone(&self.logs));
        }
    }
}

fn spawn_log_task<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    source: LogSource,
    level: LogLevel,
    reader: R,
    buffer: Arc<Mutex<LogBuffer>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut buffer) = buffer.lock() {
                buffer.push(LogLine {
                    timestamp: Utc::now(),
                    level: level.clone(),
                    source: source.clone(),
                    message: line,
                });
            }
        }
    });
}

fn render_template(
    template: &str,
    local_postgres_port: Option<u16>,
    local_s3_port: Option<u16>,
) -> Result<String> {
    let mut output = template.to_string();
    if output.contains("{local_postgres_port}") {
        let port = local_postgres_port.ok_or_else(|| {
            DesktopError::InvalidProfile("missing local_postgres_port".to_string())
        })?;
        output = output.replace("{local_postgres_port}", &port.to_string());
    }
    if output.contains("{local_s3_port}") {
        let port = local_s3_port.ok_or_else(|| {
            DesktopError::InvalidProfile("missing local_s3_port".to_string())
        })?;
        output = output.replace("{local_s3_port}", &port.to_string());
    }
    Ok(output)
}

fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(5);
    let secs = 2_u64.pow(exponent);
    Duration::from_secs(secs.min(30))
}

#[derive(Debug)]
struct LogBuffer {
    capacity: usize,
    entries: std::collections::VecDeque<LogLine>,
}

impl LogBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: std::collections::VecDeque::new(),
        }
    }

    fn push(&mut self, line: LogLine) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(line);
    }

    fn tail(&self, count: usize) -> Vec<LogLine> {
        let len = self.entries.len();
        let start = len.saturating_sub(count);
        self.entries.iter().skip(start).cloned().collect()
    }
}
