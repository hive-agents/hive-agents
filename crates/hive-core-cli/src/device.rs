use crate::util::{err, take_value, Result};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DeviceAddOptions {
    pub name: String,
    pub pubkey: String,
    pub authorized_keys: PathBuf,
    pub permit_open: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceRemoveOptions {
    pub name: String,
    pub authorized_keys: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DeviceListOptions {
    pub authorized_keys: PathBuf,
}

pub fn parse_add_args(args: &mut VecDeque<String>) -> Result<DeviceAddOptions> {
    let mut name = None;
    let mut pubkey = None;
    let mut pubkey_file = None;
    let mut authorized_keys = PathBuf::from("/home/hivec/.ssh/authorized_keys");
    let permit_open = vec!["127.0.0.1:5432".to_string(), "127.0.0.1:8333".to_string()];

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--name" => name = Some(take_value(args, "--name")?),
            "--pubkey" => pubkey = Some(take_value(args, "--pubkey")?),
            "--pubkey-file" => pubkey_file = Some(take_value(args, "--pubkey-file")?),
            "--authorized-keys" => {
                authorized_keys = PathBuf::from(take_value(args, "--authorized-keys")?)
            }
            _ => return Err(err(format!("unknown device add flag: {}", arg))),
        }
    }

    let name = name.ok_or_else(|| err("--name is required"))?;
    if name.chars().any(|c| c.is_whitespace()) {
        return Err(err("device name must not contain whitespace"));
    }

    if pubkey.is_some() && pubkey_file.is_some() {
        return Err(err("use either --pubkey or --pubkey-file, not both"));
    }

    let pubkey = match (pubkey, pubkey_file) {
        (Some(key), None) => key,
        (None, Some(path)) => fs::read_to_string(&path)
            .map_err(|e| err(format!("failed to read {}: {}", path, e)))?,
        _ => return Err(err("--pubkey or --pubkey-file is required")),
    };

    let pubkey = normalize_pubkey(&pubkey)?;

    Ok(DeviceAddOptions {
        name,
        pubkey,
        authorized_keys,
        permit_open,
    })
}

pub fn parse_remove_args(args: &mut VecDeque<String>) -> Result<DeviceRemoveOptions> {
    let mut name = None;
    let mut authorized_keys = PathBuf::from("/home/hivec/.ssh/authorized_keys");

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--name" => name = Some(take_value(args, "--name")?),
            "--authorized-keys" => {
                authorized_keys = PathBuf::from(take_value(args, "--authorized-keys")?)
            }
            _ => return Err(err(format!("unknown device remove flag: {}", arg))),
        }
    }

    let name = name.ok_or_else(|| err("--name is required"))?;
    if name.chars().any(|c| c.is_whitespace()) {
        return Err(err("device name must not contain whitespace"));
    }

    Ok(DeviceRemoveOptions {
        name,
        authorized_keys,
    })
}

pub fn parse_list_args(args: &mut VecDeque<String>) -> Result<DeviceListOptions> {
    let mut authorized_keys = PathBuf::from("/home/hivec/.ssh/authorized_keys");

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--authorized-keys" => {
                authorized_keys = PathBuf::from(take_value(args, "--authorized-keys")?)
            }
            _ => return Err(err(format!("unknown device ls flag: {}", arg))),
        }
    }

    Ok(DeviceListOptions { authorized_keys })
}

pub fn add_device(opts: DeviceAddOptions) -> Result<()> {
    ensure_parent_dir(&opts.authorized_keys)?;

    if opts.authorized_keys.exists() {
        let existing = fs::read_to_string(&opts.authorized_keys)
            .map_err(|e| err(format!("failed to read {}: {}", opts.authorized_keys.display(), e)))?;
        if existing.contains(&format!("hive-device={}", opts.name)) {
            return Err(err("device name already exists in authorized_keys"));
        }
    }

    let options = format_key_options(&opts.permit_open);
    let line = format!("{} {} hive-device={}\n", options, opts.pubkey, opts.name);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&opts.authorized_keys)
        .map_err(|e| err(format!("failed to open {}: {}", opts.authorized_keys.display(), e)))?;
    file.write_all(line.as_bytes())
        .map_err(|e| err(format!("failed to write {}: {}", opts.authorized_keys.display(), e)))?;

    set_key_permissions(&opts.authorized_keys)?;
    println!("added device {}", opts.name);
    Ok(())
}

pub fn remove_device(opts: DeviceRemoveOptions) -> Result<()> {
    if !opts.authorized_keys.exists() {
        return Err(err(format!(
            "{} does not exist",
            opts.authorized_keys.display()
        )));
    }

    let existing = fs::read_to_string(&opts.authorized_keys)
        .map_err(|e| err(format!("failed to read {}: {}", opts.authorized_keys.display(), e)))?;

    let marker = format!("hive-device={}", opts.name);
    let mut removed = 0;
    let mut kept = Vec::new();
    for line in existing.lines() {
        if line.contains(&marker) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }

    if removed == 0 {
        return Err(err("device name not found in authorized_keys"));
    }

    let mut output = kept.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    fs::write(&opts.authorized_keys, output)
        .map_err(|e| err(format!("failed to write {}: {}", opts.authorized_keys.display(), e)))?;

    println!("removed device {}", opts.name);
    Ok(())
}

pub fn list_devices(opts: DeviceListOptions) -> Result<()> {
    if !opts.authorized_keys.exists() {
        println!("no devices");
        return Ok(());
    }

    let existing = fs::read_to_string(&opts.authorized_keys)
        .map_err(|e| err(format!("failed to read {}: {}", opts.authorized_keys.display(), e)))?;

    let mut names: Vec<String> = existing
        .lines()
        .filter_map(|line| extract_device_name(line))
        .collect();

    names.sort();
    names.dedup();

    if names.is_empty() {
        println!("no devices");
        return Ok(());
    }

    for name in names {
        println!("{}", name);
    }
    Ok(())
}

fn normalize_pubkey(pubkey: &str) -> Result<String> {
    let trimmed = pubkey.trim();
    if trimmed.contains('\n') {
        return Err(err("pubkey must be a single line"));
    }
    if !(trimmed.starts_with("ssh-") || trimmed.starts_with("ecdsa-")) {
        return Err(err("pubkey must start with ssh- or ecdsa-"));
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(err("pubkey is missing key data"));
    }
    Ok(trimmed.to_string())
}

fn extract_device_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let marker = "hive-device=";
    let idx = trimmed.find(marker)?;
    let rest = &trimmed[idx + marker.len()..];
    let end = rest
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| err(format!("failed to create {}: {}", parent.display(), e)))?;
    }
    Ok(())
}

fn format_key_options(permit_open: &[String]) -> String {
    let mut options = vec![
        "no-pty".to_string(),
        "no-agent-forwarding".to_string(),
        "no-X11-forwarding".to_string(),
    ];
    for host in permit_open {
        options.push(format!("permitopen=\"{}\"", host));
    }
    options.join(",")
}

fn set_key_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .map_err(|e| err(format!("failed to set permissions on {}: {}", path.display(), e)))?;
        if let Some(parent) = path.parent() {
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(parent, perms).ok();
        }
    }
    Ok(())
}
