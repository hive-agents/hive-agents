use crate::host_key;
use crate::util::{err, take_value, Result};
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct StatusOptions {
    pub root: PathBuf,
}

pub fn parse_args(args: &mut VecDeque<String>) -> Result<StatusOptions> {
    let mut root = PathBuf::from("/opt/hive-core");

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(take_value(args, "--root")?),
            _ => return Err(err(format!("unknown status flag: {}", arg))),
        }
    }

    Ok(StatusOptions { root })
}

pub fn run(opts: StatusOptions) -> Result<()> {
    let compose_path = opts.root.join("compose.yml");
    let env_path = opts.root.join(".env");
    let format_path = opts.root.join("state").join("juicefs.format");

    println!("root: {}", opts.root.display());
    println!("compose: {}", present(&compose_path));
    println!("env: {}", present(&env_path));
    println!("juicefs formatted: {}", present(&format_path));

    println!("postgres port: {}", port_status("127.0.0.1:5432"));
    println!("s3 port: {}", port_status("127.0.0.1:8333"));
    println!("pairing port: {}", port_status("127.0.0.1:8081"));

    match docker_compose_ps(&opts.root) {
        Ok(output) => {
            if output.trim().is_empty() {
                println!("docker compose ps: no output");
            } else {
                println!("docker compose ps:\n{}", output.trim());
            }
        }
        Err(message) => println!("docker compose ps: {}", message),
    }

    match host_key::read_fingerprint() {
        Ok(Some(fingerprint)) => println!("ssh host key fingerprint: {}", fingerprint),
        Ok(None) => println!("ssh host key fingerprint: not found"),
        Err(error) => println!("ssh host key fingerprint: {}", error),
    }

    println!("connection hints:");
    println!("- ssh user: hivec");
    println!("- forwards: 127.0.0.1:5432 (postgres), 127.0.0.1:8333 (s3)");
    println!("- profile envelope: hive-core connect --host <public-host> --user hivec");
    println!("- pairing endpoint: https://<host>/hive-pair -> http://127.0.0.1:8081");
    println!("- localhost pairing: http://localhost:8081/hive-pair");
    Ok(())
}

pub fn print_fingerprint() -> Result<()> {
    match host_key::read_fingerprint() {
        Ok(Some(fingerprint)) => {
            println!("{}", fingerprint);
            Ok(())
        }
        Ok(None) => Err(err("no ssh host key found")),
        Err(error) => Err(error),
    }
}

fn present(path: &Path) -> &'static str {
    if path.exists() {
        "present"
    } else {
        "missing"
    }
}

fn port_status(addr: &str) -> &'static str {
    let socket: SocketAddr = match addr.parse() {
        Ok(addr) => addr,
        Err(_) => return "invalid",
    };

    TcpStream::connect_timeout(&socket, Duration::from_millis(500))
        .map(|_| "open")
        .unwrap_or("closed")
}

fn docker_compose_ps(root: &Path) -> std::result::Result<String, String> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("ps")
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to run docker compose: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "docker compose failed".to_string()
        } else {
            stderr
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
