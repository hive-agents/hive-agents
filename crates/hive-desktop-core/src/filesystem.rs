use std::path::Path;

use crate::errors::Result;

#[cfg(any(target_os = "macos", target_os = "windows", not(any(target_os = "linux"))))]
use crate::errors::DesktopError;

#[cfg(target_os = "linux")]
pub fn is_mounted(path: &Path) -> Result<bool> {
    let content = std::fs::read_to_string("/proc/self/mountinfo")?;
    let needle = path.to_string_lossy();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let _id = parts.next();
        let _parent = parts.next();
        let _major_minor = parts.next();
        let _root = parts.next();
        let mount_point = parts.next();
        if let Some(mount_point) = mount_point {
            let decoded = mount_point.replace("\\040", " ");
            if decoded == needle {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
pub fn is_mounted(_path: &Path) -> Result<bool> {
    Err(DesktopError::UnsupportedPlatform(
        "mount detection (macos)",
    ))
}

#[cfg(target_os = "windows")]
pub fn is_mounted(_path: &Path) -> Result<bool> {
    Err(DesktopError::UnsupportedPlatform(
        "mount detection (windows)",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn is_mounted(_path: &Path) -> Result<bool> {
    Err(DesktopError::UnsupportedPlatform("mount detection"))
}
