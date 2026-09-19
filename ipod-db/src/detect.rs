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
    /// iTunes-assigned name from `iPod_Control/iTunes/DeviceInfo` (UTF-16LE),
    /// falling back to a non-generic FAT volume label. `None` when the iPod
    /// has never been named.
    pub name: Option<String>,
    /// SysInfoExtended `FamilyID` (generation integer). `None` when the
    /// plist is missing, binary, or has no FamilyID key.
    pub family_id: Option<u32>,
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

/// Read the iTunes-assigned iPod name from `iPod_Control/iTunes/DeviceInfo`.
/// The file is UTF-16LE (optional BOM, trailing NULs). `None` if missing
/// or empty.
fn read_device_name(mount: &Path) -> Option<String> {
    let path = mount.join("iPod_Control").join("iTunes").join("DeviceInfo");
    let bytes = std::fs::read(path).ok()?;
    decode_utf16le_name(&bytes)
}

fn decode_utf16le_name(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 {
        return None;
    }
    let decoded = utf16le_to_string(bytes);
    if is_plausible_name(&decoded) {
        return Some(decoded);
    }
    // Some writers prefix a UTF-16 length (character count).
    if bytes.len() >= 4 {
        let n = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        if n > 0 && bytes.len() >= 2 + n * 2 {
            let decoded = utf16le_to_string(&bytes[2..2 + n * 2]);
            if is_plausible_name(&decoded) {
                return Some(decoded);
            }
        }
    }
    None
}

fn is_plausible_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_control())
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let bytes = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else {
        bytes
    };
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_matches('\0')
        .trim()
        .to_string()
}

/// FAT volume label from the mount directory name. Generic `IPOD` labels
/// are treated as unnamed so a model string can take over in the UI.
fn volume_name(mount: &Path) -> Option<String> {
    let name = mount.file_name()?.to_str()?.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("ipod") {
        None
    } else {
        Some(name.to_string())
    }
}

/// SysInfoExtended `FamilyID` from the XML plist. Binary plists (`bplist`)
/// are skipped — we don't pull in a plist crate for one integer.
fn read_family_id(mount: &Path) -> Option<u32> {
    let path = mount
        .join("iPod_Control")
        .join("Device")
        .join("SysInfoExtended");
    let bytes = std::fs::read(path).ok()?;
    let xml = std::str::from_utf8(&bytes).ok()?;
    plist_integer(xml, "FamilyID")
}

/// Pull the integer immediately following `<key>NAME</key>` in a plist.
pub(crate) fn plist_integer(xml: &str, key: &str) -> Option<u32> {
    let needle = format!("<key>{key}</key>");
    let rest = xml.split(&needle).nth(1)?;
    let start = rest.find("<integer>")? + "<integer>".len();
    let end = rest[start..].find("</integer>")?;
    rest[start..start + end].trim().parse().ok()
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
            let name = read_device_name(&candidate).or_else(|| volume_name(&candidate));
            let family_id = read_family_id(&candidate);
            results.push(DetectedIpod {
                mount_point: candidate,
                model,
                serial,
                firmware_version: firmware,
                name,
                family_id,
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

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn test_read_device_name_utf16le() {
        let tmp = TempDir::new().unwrap();
        let itunes = tmp.path().join("iPod_Control").join("iTunes");
        std::fs::create_dir_all(&itunes).unwrap();
        std::fs::write(itunes.join("DeviceInfo"), utf16le("Dublin's iPod")).unwrap();

        assert_eq!(
            read_device_name(tmp.path()).as_deref(),
            Some("Dublin's iPod")
        );
    }

    #[test]
    fn test_read_device_name_strips_bom_and_nuls() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(utf16le("Classic"));
        bytes.extend_from_slice(&[0, 0]);
        assert_eq!(decode_utf16le_name(&bytes).as_deref(), Some("Classic"));
    }

    #[test]
    fn test_read_device_name_length_prefixed() {
        let name = utf16le("Nano");
        let mut bytes = ((name.len() / 2) as u16).to_le_bytes().to_vec();
        bytes.extend(name);
        assert_eq!(decode_utf16le_name(&bytes).as_deref(), Some("Nano"));
    }

    #[test]
    fn test_volume_name_skips_generic_ipod() {
        assert_eq!(volume_name(Path::new("/media/user/IPOD")), None);
        assert_eq!(
            volume_name(Path::new("/Volumes/DUBLIN IPOD")).as_deref(),
            Some("DUBLIN IPOD")
        );
    }

    #[test]
    fn test_plist_integer_family_id() {
        let xml = r#"<?xml version="1.0"?>
<plist><dict>
<key>FireWireGUID</key>
<string>000A2700</string>
<key>FamilyID</key>
<integer>31</integer>
</dict></plist>"#;
        assert_eq!(plist_integer(xml, "FamilyID"), Some(31));
        assert_eq!(plist_integer(xml, "Missing"), None);
    }

    #[test]
    fn test_read_family_id_from_sysinfo_extended() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(
            device_dir.join("SysInfoExtended"),
            "<plist><dict><key>FamilyID</key>\n<integer>26</integer></dict></plist>",
        )
        .unwrap();
        assert_eq!(read_family_id(tmp.path()), Some(26));
    }

    #[test]
    fn test_read_family_id_skips_binary_plist() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(device_dir.join("SysInfoExtended"), b"bplist00\x00\x01").unwrap();
        assert_eq!(read_family_id(tmp.path()), None);
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
