use crate::util::{err, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn read_fingerprint() -> Result<Option<String>> {
    let key_path = match find_host_key_path() {
        Some(path) => path,
        None => return Ok(None),
    };

    let output = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(&key_path)
        .output()
        .map_err(|e| err(format!("failed to run ssh-keygen: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(err(if stderr.is_empty() {
            "ssh-keygen failed".to_string()
        } else {
            stderr
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let fingerprint = stdout
        .split_whitespace()
        .find(|part| part.starts_with("SHA256:"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| stdout.trim().to_string());

    Ok(Some(fingerprint))
}

pub fn scan_fingerprint(host: &str, port: u16) -> Result<String> {
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
        lines.push(trimmed.to_string());
    }
    if lines.is_empty() {
        return Err(err("ssh-keyscan returned no host keys"));
    }

    let preferred = select_key_line(&lines);
    fingerprint_from_key_line(preferred)
}

fn find_host_key_path() -> Option<PathBuf> {
    let candidates = [
        "/etc/ssh/ssh_host_ed25519_key.pub",
        "/etc/ssh/ssh_host_ecdsa_key.pub",
        "/etc/ssh/ssh_host_rsa_key.pub",
    ];

    for path in candidates {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn select_key_line(lines: &[String]) -> &str {
    let preferred = ["ssh-ed25519", "ecdsa-sha2", "ssh-rsa"];
    for key_type in preferred {
        if let Some(line) = lines.iter().find(|line| line.contains(key_type)) {
            return line;
        }
    }
    &lines[0]
}

fn fingerprint_from_key_line(line: &str) -> Result<String> {
    let mut child = Command::new("ssh-keygen")
        .arg("-lf")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| err(format!("failed to run ssh-keygen: {}", e)))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| err("ssh-keygen stdin unavailable"))?;
        stdin
            .write_all(line.as_bytes())
            .map_err(|e| err(format!("failed to write host key: {}", e)))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| err(format!("ssh-keygen failed: {}", e)))?;
    if !output.status.success() {
        return Err(err("ssh-keygen returned error"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let fingerprint = stdout
        .split_whitespace()
        .find(|part| part.starts_with("SHA256:"))
        .ok_or_else(|| err("failed to parse host key fingerprint"))?;
    Ok(fingerprint.to_string())
}
