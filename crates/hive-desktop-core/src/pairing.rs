use std::process::Command;
use std::time::Duration;

use hive_protocol::{PairError, PairRequest, PairResponse, PairingEnvelope};
use reqwest::{Client, Url};

use crate::{DesktopError, Result};

pub struct PairingOutcome {
    pub response: PairResponse,
    pub device_name: String,
    pub device_public_key: String,
    pub device_private_key: String,
}

pub async fn pair_envelope(
    envelope: &PairingEnvelope,
    device_name: Option<String>,
) -> Result<PairingOutcome> {
    let device_name = device_name
        .map(|name| sanitize_device_name(&name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(default_device_name);

    let (private_key, public_key) = generate_device_keypair(&device_name)?;
    let public_key = public_key.trim().to_string();

    let response = pair_with_core(envelope, &device_name, &public_key).await?;
    if response.profile_id != envelope.profile.profile_id {
        return Err(DesktopError::Pairing(
            "pairing response profile_id mismatch".to_string(),
        ));
    }

    Ok(PairingOutcome {
        response,
        device_name,
        device_public_key: public_key,
        device_private_key: private_key,
    })
}

pub async fn pair_with_core(
    envelope: &PairingEnvelope,
    device_name: &str,
    device_pubkey: &str,
) -> Result<PairResponse> {
    if envelope.pair_url.trim().is_empty() {
        return Err(DesktopError::Pairing("pair_url is missing".to_string()));
    }

    let pair_url = parse_pair_url(&envelope.pair_url)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| DesktopError::Pairing(format!("failed to build http client: {e}")))?;

    let request = PairRequest {
        otp: envelope.otp.clone(),
        device_name: device_name.to_string(),
        device_pubkey: device_pubkey.trim().to_string(),
    };

    let response = client
        .post(pair_url)
        .json(&request)
        .send()
        .await
        .map_err(|e| DesktopError::Pairing(format!("pairing request failed: {e}")))?;

    let status = response.status();
    if status.is_success() {
        return response
            .json::<PairResponse>()
            .await
            .map_err(|e| DesktopError::Pairing(format!("failed to parse pairing response: {e}")));
    }

    let body = response.text().await.unwrap_or_default();
    if let Ok(err) = serde_json::from_str::<PairError>(&body) {
        return Err(DesktopError::Pairing(format!(
            "pairing failed: {} ({})",
            err.message, err.error
        )));
    }

    Err(DesktopError::Pairing(format!(
        "pairing failed: http {}",
        status.as_u16()
    )))
}

pub fn default_device_name() -> String {
    if let Ok(host) = hostname::get() {
        if let Some(host) = host.to_str() {
            return sanitize_device_name(host);
        }
    }
    "hive-desktop".to_string()
}

pub fn generate_device_keypair(comment: &str) -> Result<(String, String)> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut key_path = None;
    for attempt in 0..100 {
        let candidate = base.join(format!("hive-agents-device-key-{pid}-{now}-{attempt}"));
        if !candidate.exists() {
            key_path = Some(candidate);
            break;
        }
    }
    let key_path = key_path.ok_or_else(|| {
        DesktopError::Pairing("failed to create temp key path".to_string())
    })?;

    let status = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-f")
        .arg(&key_path)
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(comment)
        .status()
        .map_err(|e| DesktopError::Pairing(format!("ssh-keygen failed: {e}")))?;
    if !status.success() {
        return Err(DesktopError::Pairing(
            "ssh-keygen exited with error".to_string(),
        ));
    }

    let private_key = std::fs::read_to_string(&key_path)
        .map_err(|e| DesktopError::Pairing(format!("failed to read {}: {e}", key_path.display())))?;
    let public_key_path = key_path.with_extension("pub");
    let public_key = std::fs::read_to_string(&public_key_path).map_err(|e| {
        DesktopError::Pairing(format!(
            "failed to read {}: {e}",
            public_key_path.display()
        ))
    })?;

    let _ = std::fs::remove_file(&key_path);
    let _ = std::fs::remove_file(&public_key_path);

    Ok((private_key, public_key))
}

fn sanitize_device_name(name: &str) -> String {
    let joined = name.split_whitespace().collect::<Vec<_>>().join("-");
    if joined.is_empty() {
        "hive-desktop".to_string()
    } else {
        joined
    }
}

fn parse_pair_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|e| DesktopError::Pairing(format!("pair_url invalid: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| DesktopError::Pairing("pair_url is missing host".to_string()))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" => {
            if is_localhost(host) {
                Ok(url)
            } else {
                Err(DesktopError::Pairing(
                    "pair_url must use https for non-local hosts".to_string(),
                ))
            }
        }
        _ => Err(DesktopError::Pairing(
            "pair_url must use http or https".to_string(),
        )),
    }
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}
