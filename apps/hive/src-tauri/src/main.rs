#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use directories::{BaseDirs, ProjectDirs};
use hive_desktop_core::{
    pair_envelope, DesktopError, LogLine, SecretStore, Status, SupervisorConfig, SupervisorHandle,
};
use hive_protocol::{
    CacheSettings, LocalSettings, Mountpoint, PairingEnvelope, Platform, Profile, ProfileId,
    RuntimeSettings,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

struct AppPaths {
    profiles_dir: PathBuf,
    settings_dir: PathBuf,
    secrets_dir: PathBuf,
    pairing_dir: PathBuf,
    known_hosts_path: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    fn new() -> Result<Self, String> {
        let dirs = ProjectDirs::from("com", "hive-agents", "Hive")
            .or_else(|| ProjectDirs::from("", "", "Hive"));

        let (data_dir, config_dir, cache_dir) = if let Some(dirs) = dirs {
            (
                dirs.data_dir().to_path_buf(),
                dirs.config_dir().to_path_buf(),
                dirs.cache_dir().to_path_buf(),
            )
        } else {
            let base = std::env::current_dir()
                .map_err(|e| format!("failed to resolve data dir: {e}"))?
                .join(".hive");
            (base.clone(), base.clone(), base.join("cache"))
        };

        let profiles_dir = data_dir.join("profiles");
        let settings_dir = data_dir.join("settings");
        let secrets_dir = data_dir.join("secrets");
        let pairing_dir = data_dir.join("pairing");
        let known_hosts_path = config_dir.join("known_hosts");

        ensure_dir(&profiles_dir)?;
        ensure_dir(&settings_dir)?;
        ensure_dir(&secrets_dir)?;
        ensure_dir(&pairing_dir)?;
        ensure_dir(&cache_dir)?;
        if let Some(parent) = known_hosts_path.parent() {
            ensure_dir(parent)?;
        }

        Ok(Self {
            profiles_dir,
            settings_dir,
            secrets_dir,
            pairing_dir,
            known_hosts_path,
            cache_dir,
        })
    }
}

struct AppState {
    paths: AppPaths,
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

struct SessionEntry {
    handle: SupervisorHandle,
}

#[derive(Debug, Serialize)]
struct ProfileSummary {
    profile_id: String,
    display_name: String,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct LocalSettingsPatch {
    mountpoint: Option<MountpointPatch>,
    cache: Option<CachePatch>,
}

#[derive(Debug, Deserialize)]
struct MountpointPatch {
    path: Option<String>,
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct CachePatch {
    cache_dir: Option<String>,
    cache_size_mib: Option<u32>,
}

#[tauri::command]
fn profiles_list(state: State<'_, AppState>) -> Result<Vec<ProfileSummary>, String> {
    let mut profiles = Vec::new();
    for entry in fs::read_dir(&state.paths.profiles_dir)
        .map_err(|e| format!("failed to read profiles: {e}"))?
    {
        let entry = entry.map_err(|e| format!("failed to read profile entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let profile = load_profile(&path)?;
        profiles.push(ProfileSummary {
            profile_id: profile.profile_id.0.to_string(),
            display_name: profile.display_name.clone(),
            created_at: profile.created_at.to_rfc3339(),
        });
    }
    profiles.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(profiles)
}

#[tauri::command]
async fn profile_import(
    state: State<'_, AppState>,
    json: String,
) -> Result<String, String> {
    let envelope = parse_pairing_envelope(&json)?;
    let profile = envelope.profile.clone();
    let pairing = pair_envelope(&envelope, None)
        .await
        .map_err(|e| e.to_string())?;

    let mut secrets = pairing.response.secrets;
    secrets.insert(
        profile.ssh.identity_key_ref.as_str().to_string(),
        pairing.device_private_key,
    );

    let profile_id = profile.profile_id.clone();
    let profile_path = profile_path(&state.paths, &profile_id);
    write_profile(&profile_path, &profile)?;

    let secrets_path = secrets_path(&state.paths, &profile_id);
    merge_and_write_secrets(&secrets_path, secrets)?;

    let pairing_path = pairing_path(&state.paths, &profile_id);
    write_pairing_envelope(&pairing_path, &envelope)?;

    ensure_local_settings(&state.paths, &profile)?;

    Ok(profile_id.0.to_string())
}

#[tauri::command]
fn profile_export(state: State<'_, AppState>, profile_id: String) -> Result<String, String> {
    let profile_id = parse_profile_id(&profile_id)?;
    let path = profile_path(&state.paths, &profile_id);
    let profile = load_profile(&path)?;
    serde_json::to_string(&profile).map_err(|e| format!("failed to serialize profile: {e}"))
}

#[tauri::command]
fn get_local_settings(state: State<'_, AppState>, profile_id: String) -> Result<LocalSettings, String> {
    let profile_id = parse_profile_id(&profile_id)?;
    let profile_path = profile_path(&state.paths, &profile_id);
    let profile = load_profile(&profile_path)?;
    let settings_path = settings_path(&state.paths, &profile_id);

    if settings_path.exists() {
        return load_local_settings(&settings_path);
    }

    let settings = default_local_settings(&state.paths, &profile);
    write_local_settings(&settings_path, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn set_local_settings(
    state: State<'_, AppState>,
    profile_id: String,
    patch: LocalSettingsPatch,
) -> Result<(), String> {
    let profile_id = parse_profile_id(&profile_id)?;
    let settings_path = settings_path(&state.paths, &profile_id);
    let mut settings = if settings_path.exists() {
        load_local_settings(&settings_path)?
    } else {
        LocalSettings {
            profile_id: profile_id.clone(),
            mountpoint: Mountpoint {
                platform: detect_platform(),
                path: default_mountpoint(),
            },
            cache: CacheSettings {
                cache_dir: default_cache_dir(&state.paths, &profile_id),
                cache_size_mib: 10240,
            },
            runtime: RuntimeSettings::default(),
        }
    };

    if let Some(mountpoint) = patch.mountpoint {
        if let Some(path) = mountpoint.path {
            if path.trim().is_empty() {
                return Err("mountpoint path cannot be empty".to_string());
            }
            settings.mountpoint.path = PathBuf::from(path);
        }
        if let Some(platform) = mountpoint.platform {
            settings.mountpoint.platform = platform;
        }
    }

    if let Some(cache) = patch.cache {
        if let Some(path) = cache.cache_dir {
            if path.trim().is_empty() {
                return Err("cache dir cannot be empty".to_string());
            }
            settings.cache.cache_dir = PathBuf::from(path);
        }
        if let Some(size) = cache.cache_size_mib {
            if size == 0 {
                return Err("cache size must be greater than zero".to_string());
            }
            settings.cache.cache_size_mib = size;
        }
    }

    settings
        .validate()
        .map_err(|e| format!("settings invalid: {e}"))?;
    write_local_settings(&settings_path, &settings)?;
    Ok(())
}

#[tauri::command]
async fn connect(state: State<'_, AppState>, profile_id: String) -> Result<String, String> {
    let profile_id = parse_profile_id(&profile_id)?;
    let profile_path = profile_path(&state.paths, &profile_id);
    let profile = load_profile(&profile_path)?;

    let settings_path = settings_path(&state.paths, &profile_id);
    let settings = if settings_path.exists() {
        load_local_settings(&settings_path)?
    } else {
        let defaults = default_local_settings(&state.paths, &profile);
        write_local_settings(&settings_path, &defaults)?;
        defaults
    };

    let secrets_path = secrets_path(&state.paths, &profile_id);
    let secrets = read_secrets(&secrets_path)?;
    let missing = missing_secrets(&profile, &secrets);
    if !missing.is_empty() {
        return Err(format!(
            "missing secrets: {} (import a pairing envelope)",
            missing.join(", ")
        ));
    }

    let mut config = SupervisorConfig::default();
    config.known_hosts_path = state.paths.known_hosts_path.clone();

    let handle = SupervisorHandle::spawn(
        profile,
        settings,
        std::sync::Arc::new(FileSecretStore { secrets }),
        config,
    );
    let session_id = Uuid::new_v4().to_string();
    {
        let mut sessions = state
            .sessions
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        if !sessions.is_empty() {
            return Err("another session is already active".to_string());
        }
        sessions.insert(
            session_id.clone(),
            SessionEntry {
                handle: handle.clone(),
            },
        );
    }

    if let Err(err) = handle.connect().await {
        let mut sessions = state
            .sessions
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        sessions.remove(&session_id);
        return Err(format!("connect failed: {err}"));
    }

    Ok(session_id)
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let entry = {
        let mut sessions = state
            .sessions
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        sessions.remove(&session_id)
    };

    let Some(entry) = entry else {
        return Err("session not found".to_string());
    };

    if let Err(err) = entry.handle.disconnect().await {
        entry.handle.force_disconnect().await.ok();
        return Err(format!("disconnect failed: {err}"));
    }

    Ok(())
}

#[tauri::command]
fn status(state: State<'_, AppState>, session_id: String) -> Result<Status, String> {
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| "session lock poisoned".to_string())?;
    let entry = sessions
        .get(&session_id)
        .ok_or_else(|| "session not found".to_string())?;
    Ok(entry.handle.status())
}

#[tauri::command]
fn logs_tail(state: State<'_, AppState>, session_id: String, n: usize) -> Result<Vec<LogLine>, String> {
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| "session lock poisoned".to_string())?;
    let entry = sessions
        .get(&session_id)
        .ok_or_else(|| "session not found".to_string())?;
    Ok(entry.handle.logs_tail(n))
}

fn parse_pairing_envelope(payload: &str) -> Result<PairingEnvelope, String> {
    let envelope: PairingEnvelope = serde_json::from_str(payload)
        .map_err(|e| format!("invalid pairing envelope json: {e}"))?;
    envelope
        .profile
        .validate()
        .map_err(|e| format!("profile invalid: {e}"))?;
    Ok(envelope)
}

fn parse_profile_id(value: &str) -> Result<ProfileId, String> {
    let id = Uuid::parse_str(value).map_err(|e| format!("invalid profile id: {e}"))?;
    Ok(ProfileId(id))
}

fn load_profile(path: &Path) -> Result<Profile, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("failed to parse profile: {e}"))
}

fn write_profile(path: &Path, profile: &Profile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(profile)
        .map_err(|e| format!("failed to serialize profile: {e}"))?;
    fs::write(path, json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn load_local_settings(path: &Path) -> Result<LocalSettings, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("failed to parse settings: {e}"))
}

fn write_local_settings(path: &Path, settings: &LocalSettings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("failed to serialize settings: {e}"))?;
    fs::write(path, json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn default_local_settings(paths: &AppPaths, profile: &Profile) -> LocalSettings {
    LocalSettings {
        profile_id: profile.profile_id.clone(),
        mountpoint: Mountpoint {
            platform: detect_platform(),
            path: default_mountpoint(),
        },
        cache: CacheSettings {
            cache_dir: default_cache_dir(paths, &profile.profile_id),
            cache_size_mib: 10240,
        },
        runtime: RuntimeSettings::default(),
    }
}

fn ensure_local_settings(paths: &AppPaths, profile: &Profile) -> Result<(), String> {
    let path = settings_path(paths, &profile.profile_id);
    if path.exists() {
        return Ok(());
    }
    let settings = default_local_settings(paths, profile);
    write_local_settings(&path, &settings)
}

fn default_cache_dir(paths: &AppPaths, profile_id: &ProfileId) -> PathBuf {
    paths.cache_dir.join(profile_id.0.to_string())
}

fn default_mountpoint() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base.home_dir().join("Hive");
    }
    PathBuf::from("Hive")
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

fn profile_path(paths: &AppPaths, profile_id: &ProfileId) -> PathBuf {
    paths
        .profiles_dir
        .join(format!("{}.json", profile_id.0))
}

fn settings_path(paths: &AppPaths, profile_id: &ProfileId) -> PathBuf {
    paths
        .settings_dir
        .join(format!("{}.json", profile_id.0))
}

fn secrets_path(paths: &AppPaths, profile_id: &ProfileId) -> PathBuf {
    paths
        .secrets_dir
        .join(format!("{}.json", profile_id.0))
}

fn pairing_path(paths: &AppPaths, profile_id: &ProfileId) -> PathBuf {
    paths
        .pairing_dir
        .join(format!("{}.json", profile_id.0))
}

fn read_secrets(path: &Path) -> Result<BTreeMap<String, String>, String> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("failed to parse secrets: {e}"))
}

fn merge_and_write_secrets(
    path: &Path,
    updates: BTreeMap<String, String>,
) -> Result<(), String> {
    let mut secrets = read_secrets(path)?;
    for (key, value) in updates {
        secrets.insert(key, value);
    }
    write_secrets(path, &secrets)
}

fn write_secrets(path: &Path, secrets: &BTreeMap<String, String>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(secrets)
        .map_err(|e| format!("failed to serialize secrets: {e}"))?;
    fs::write(path, json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms).ok();
    }
    Ok(())
}

fn write_pairing_envelope(path: &Path, envelope: &PairingEnvelope) -> Result<(), String> {
    let json = serde_json::to_string_pretty(envelope)
        .map_err(|e| format!("failed to serialize pairing envelope: {e}"))?;
    fs::write(path, json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms).ok();
    }
    Ok(())
}

fn missing_secrets(profile: &Profile, secrets: &BTreeMap<String, String>) -> Vec<String> {
    let required = [
        profile.ssh.identity_key_ref.as_str(),
        profile.juicefs.meta.password_ref.as_str(),
        profile.juicefs.object.access_key_ref.as_str(),
        profile.juicefs.object.secret_key_ref.as_str(),
    ];
    let mut missing = Vec::new();
    for key in required {
        if secrets.get(key).map(|value| value.trim().is_empty()).unwrap_or(true) {
            missing.push(key.to_string());
        }
    }
    missing
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))
}

struct FileSecretStore {
    secrets: BTreeMap<String, String>,
}

impl SecretStore for FileSecretStore {
    fn resolve_text(&self, secret: &hive_protocol::SecretRef) -> Result<String, DesktopError> {
        self.secrets
            .get(secret.as_str())
            .cloned()
            .ok_or_else(|| DesktopError::SecretNotFound(secret.as_str().to_string()))
    }

    fn resolve_path(&self, secret: &hive_protocol::SecretRef) -> Result<PathBuf, DesktopError> {
        Err(DesktopError::SecretNotFound(format!(
            "path resolution not supported: {}",
            secret.as_str()
        )))
    }
}

fn main() {
    let paths = AppPaths::new().expect("failed to initialize paths");
    let state = AppState {
        paths,
        sessions: Mutex::new(HashMap::new()),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            profiles_list,
            profile_import,
            profile_export,
            get_local_settings,
            set_local_settings,
            connect,
            disconnect,
            status,
            logs_tail,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
