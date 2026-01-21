use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use hive_protocol::StorageKind;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::errors::{DesktopError, Result};
use crate::filesystem;

#[derive(Clone, Debug)]
pub struct MountEnv {
    pub meta_password: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Debug)]
pub struct MountSession {
    mountpoint: PathBuf,
    juicefs_path: PathBuf,
    child: Child,
}

impl MountSession {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub async fn wait_mounted(&self, timeout: Duration, interval: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            match filesystem::is_mounted(&self.mountpoint) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(DesktopError::UnsupportedPlatform(_)) => return Ok(()),
                Err(err) => return Err(err),
            }

            if start.elapsed() >= timeout {
                return Err(DesktopError::Timeout("waiting for mount"));
            }
            tokio::time::sleep(interval).await;
        }
    }

    pub async fn unmount(&mut self, force: bool) -> Result<()> {
        let mut cmd = Command::new(&self.juicefs_path);
        cmd.arg("umount");
        if force {
            cmd.arg("--force");
        }
        cmd.arg(&self.mountpoint);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let status = child.wait().await?;
        if !status.success() {
            return Err(DesktopError::Mount(format!(
                "juicefs umount failed: {status}"
            )));
        }

        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.child.kill();
        let _ = self.child.wait().await;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn start_mount(
    juicefs_path: &Path,
    mountpoint: &Path,
    meta_url: &str,
    bucket_url: &str,
    cache_dir: &Path,
    cache_size_mib: u32,
    storage: StorageKind,
    env: MountEnv,
) -> Result<MountSession> {
    std::fs::create_dir_all(mountpoint)?;

    let mut cmd = Command::new(juicefs_path);
    cmd.arg("mount")
        .arg("--cache-dir")
        .arg(cache_dir)
        .arg("--cache-size")
        .arg(cache_size_mib.to_string())
        .arg("--storage")
        .arg(storage_arg(storage))
        .arg("--bucket")
        .arg(bucket_url)
        .arg(meta_url)
        .arg(mountpoint);
    cmd.env("META_PASSWORD", env.meta_password);
    cmd.env("ACCESS_KEY", env.access_key);
    cmd.env("SECRET_KEY", env.secret_key);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            DesktopError::Mount(format!(
                "juicefs binary not found at {}",
                juicefs_path.display()
            ))
        } else {
            err.into()
        }
    })?;

    Ok(MountSession {
        mountpoint: mountpoint.to_path_buf(),
        juicefs_path: juicefs_path.to_path_buf(),
        child,
    })
}

fn storage_arg(storage: StorageKind) -> &'static str {
    match storage {
        StorageKind::S3 => "s3",
    }
}
