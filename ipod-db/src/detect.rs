use std::path::{Path, PathBuf};

/// Information about a detected iPod.
#[derive(Debug, Clone)]
pub struct DetectedIpod {
    /// Mount point path (e.g. /media/user/IPOD).
    pub mount_point: PathBuf,
    /// iPod model string from SysInfo, if available.
    pub model: Option<String>,
    /// Serial number from SysInfo, if available.
    pub serial: Option<String>,
    /// Firmware version from SysInfo, if available.
    pub firmware_version: Option<String>,
}

/// Check if a path looks like an iPod mount point.
fn is_ipod_mount(path: &Path) -> bool {
    path.join("iPod_Control").is_dir()
}

/// Try to parse SysInfo from iPod_Control/Device/SysInfo.
fn read_sysinfo(mount: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let sysinfo_path = mount.join("iPod_Control").join("Device").join("SysInfo");
    let contents = match std::fs::read_to_string(sysinfo_path) {
        Ok(c) => c,
        Err(_) => return (None, None, None),
    };

    let mut model = None;
    let mut serial = None;
    let mut firmware = None;

    for line in contents.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().to_string();
            match key {
                "ModelNumStr" => model = Some(value),
                "pszSerialNumber" | "FirewireGuid" if serial.is_none() => {
                    serial = Some(value);
                }
                "visibleBuildID" | "buildID" if firmware.is_none() => {
                    firmware = Some(value);
                }
                _ => {}
            }
        }
    }

    (model, serial, firmware)
}

/// Scan common mount points for connected iPods.
///
/// Checks `/media/$USER/`, `/mnt/`, and `/run/media/$USER/` for directories
/// containing `iPod_Control/`.
pub fn detect_ipods() -> Vec<DetectedIpod> {
    let mut results = Vec::new();
    let mut searched = std::collections::HashSet::new();

    let user = std::env::var("USER").unwrap_or_default();

    let mut candidates: Vec<PathBuf> = Vec::new();

    // /media/$USER/*/
    if !user.is_empty() {
        let media_user = PathBuf::from("/media").join(&user);
        if let Ok(entries) = std::fs::read_dir(&media_user) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        }
    }

    // /run/media/$USER/*/
    if !user.is_empty() {
        let run_media_user = PathBuf::from("/run/media").join(&user);
        if let Ok(entries) = std::fs::read_dir(&run_media_user) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        }
    }

    // /mnt/*/
    if let Ok(entries) = std::fs::read_dir("/mnt") {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                candidates.push(p);
            }
        }
    }

    // /Volumes/*/ (macOS)
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                candidates.push(p);
            }
        }
    }

    // Also check IPOD_MOUNT_PATH env var for explicit override.
    if let Ok(explicit) = std::env::var("IPOD_MOUNT_PATH") {
        let p = PathBuf::from(&explicit);
        if p.is_dir() {
            candidates.insert(0, p);
        }
    }

    for candidate in candidates {
        let canonical = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if !searched.insert(canonical.clone()) {
            continue;
        }
        if is_ipod_mount(&candidate) {
            let (model, serial, firmware) = read_sysinfo(&candidate);
            results.push(DetectedIpod {
                mount_point: candidate,
                model,
                serial,
                firmware_version: firmware,
            });
        }
    }

    results
}

/// Detect a single iPod, returning the first one found.
pub fn detect_ipod() -> crate::Result<DetectedIpod> {
    detect_ipods()
        .into_iter()
        .next()
        .ok_or(crate::IpodDbError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_ipod_mount() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_ipod_mount(tmp.path()));

        std::fs::create_dir_all(tmp.path().join("iPod_Control")).unwrap();
        assert!(is_ipod_mount(tmp.path()));
    }

    #[test]
    fn test_read_sysinfo() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(
            device_dir.join("SysInfo"),
            "ModelNumStr: MA002\npszSerialNumber: ABC123\nvisibleBuildID: 1.3.0\n",
        )
        .unwrap();

        let (model, serial, fw) = read_sysinfo(tmp.path());
        assert_eq!(model.as_deref(), Some("MA002"));
        assert_eq!(serial.as_deref(), Some("ABC123"));
        assert_eq!(fw.as_deref(), Some("1.3.0"));
    }

    #[test]
    fn test_detect_with_env_override() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("iPod_Control")).unwrap();

        // Safety: test is single-threaded for this env var; suppressing deprecation
        // lint since we need to test the env-based detection path.
        #[allow(deprecated)]
        {
            std::env::set_var("IPOD_MOUNT_PATH", tmp.path());
            let results = detect_ipods();
            std::env::remove_var("IPOD_MOUNT_PATH");

            assert!(!results.is_empty());
            assert_eq!(results[0].mount_point, tmp.path());
        }
    }
}
