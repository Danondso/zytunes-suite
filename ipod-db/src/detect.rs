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
    /// `boardHwSwInterfaceRev` gestalt from SysInfo (e.g. `0x000B0005`).
    pub gestalt: Option<u32>,
    /// Post-2006 firmware creates `SysInfo` at 0 bytes. Combined with USB
    /// PID this distinguishes Video 5.5G from 5G.
    pub sysinfo_empty: bool,
    /// Mount filesystem, e.g. `FAT` or `HFS+ (read-only)`.
    pub volume_format: Option<String>,
}

/// Check if a path looks like an iPod mount point.
fn is_ipod_mount(path: &Path) -> bool {
    path.join("iPod_Control").is_dir()
}

/// Parsed `iPod_Control/Device/SysInfo` fields we care about for identity.
struct SysInfoFields {
    model: Option<String>,
    serial: Option<String>,
    firmware: Option<String>,
    gestalt: Option<u32>,
    empty: bool,
}

/// Try to parse SysInfo from iPod_Control/Device/SysInfo.
fn read_sysinfo(mount: &Path) -> SysInfoFields {
    let sysinfo_path = mount.join("iPod_Control").join("Device").join("SysInfo");
    let contents = match std::fs::read_to_string(&sysinfo_path) {
        Ok(c) => c,
        Err(_) => {
            return SysInfoFields {
                model: None,
                serial: None,
                firmware: None,
                gestalt: None,
                empty: !sysinfo_path.exists(),
            };
        }
    };

    let mut fields = SysInfoFields {
        model: None,
        serial: None,
        firmware: None,
        gestalt: None,
        empty: contents.trim().is_empty(),
    };

    for line in contents.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key {
                "ModelNumStr" => fields.model = Some(value.to_string()),
                "pszSerialNumber" | "FirewireGuid" if fields.serial.is_none() => {
                    fields.serial = Some(value.to_string());
                }
                "visibleBuildID" | "buildID" if fields.firmware.is_none() => {
                    fields.firmware = Some(value.to_string());
                }
                "boardHwSwInterfaceRev" if fields.gestalt.is_none() => {
                    fields.gestalt = parse_sysinfo_hex(value);
                }
                _ => {}
            }
        }
    }
    if fields.model.is_none()
        && fields.serial.is_none()
        && fields.firmware.is_none()
        && fields.gestalt.is_none()
    {
        fields.empty = true;
    }
    fields
}

/// `0x000B0005 (0.0.11 5)` → `0x000B0005`.
fn parse_sysinfo_hex(value: &str) -> Option<u32> {
    let token = value.split_whitespace().next()?;
    let token = token.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(token, 16).ok()
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

/// True for names that are not a user-assigned iPod name: empty, generic
/// FAT labels (`IPOD`, `NO NAME`, `UNTITLED`), or Linux automount IDs
/// (`1234-5678`, GPT/UUID folder names under `/run/media/$USER/`).
pub fn is_generic_ipod_name(name: &str) -> bool {
    let n = name.trim();
    n.is_empty()
        || n.eq_ignore_ascii_case("ipod")
        || n.eq_ignore_ascii_case("no name")
        || n.eq_ignore_ascii_case("untitled")
        || n.eq_ignore_ascii_case("usb disk")
        || n.eq_ignore_ascii_case("usb drive")
        || n.eq_ignore_ascii_case("removable disk")
        || is_volume_uuid(n)
}

/// FAT volume serial (`1234-5678`) or a hex-and-hyphen UUID, as used for
/// unlabeled udisks mount directories.
fn is_volume_uuid(name: &str) -> bool {
    name.len() >= 9 && name.contains('-') && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// iTunes DeviceInfo name, else a non-generic volume label. Generic FAT
/// labels and automount UUIDs yield `None` so the UI can show a model string.
fn user_assigned_name(mount: &Path) -> Option<String> {
    read_device_name(mount)
        .filter(|n| !is_generic_ipod_name(n))
        .or_else(|| volume_name(mount))
}

fn volume_name(mount: &Path) -> Option<String> {
    let name = mount.file_name()?.to_str()?.trim();
    if is_generic_ipod_name(name) {
        None
    } else {
        Some(name.to_string())
    }
}

/// SysInfoExtended XML from the on-disk plist. Binary plists (`bplist`)
/// are skipped — we don't pull in a plist crate for one integer.
fn read_sysinfo_extended_xml(mount: &Path) -> Option<String> {
    let path = mount
        .join("iPod_Control")
        .join("Device")
        .join("SysInfoExtended");
    let bytes = std::fs::read(path).ok()?;
    std::str::from_utf8(&bytes).ok().map(str::to_string)
}

/// Pull the integer immediately following `<key>NAME</key>` in a plist.
pub(crate) fn plist_integer(xml: &str, key: &str) -> Option<u32> {
    let needle = format!("<key>{key}</key>");
    let rest = xml.split(&needle).nth(1)?;
    let start = rest.find("<integer>")? + "<integer>".len();
    let end = rest[start..].find("</integer>")?;
    rest[start..start + end].trim().parse().ok()
}

fn plist_string(xml: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let rest = xml.split(&needle).nth(1)?;
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    let s = rest[start..start + end].trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn apply_sysinfo_extended_xml(
    xml: &str,
    family_id: &mut Option<u32>,
    serial: &mut Option<String>,
    firmware: &mut Option<String>,
) {
    if family_id.is_none() {
        *family_id = plist_integer(xml, "FamilyID");
    }
    if serial.is_none() {
        *serial = plist_string(xml, "FireWireGUID")
            .or_else(|| plist_string(xml, "FirewireGuid"))
            .or_else(|| plist_string(xml, "SerialNumber"));
    }
    if firmware.is_none() {
        *firmware =
            plist_string(xml, "VisibleBuildID").or_else(|| plist_string(xml, "visibleBuildID"));
    }
}

fn pretty_fstype(fs: &str) -> String {
    match fs {
        "vfat" | "msdos" | "fat" | "exfat" => "FAT".into(),
        "hfsplus" => "HFS+".into(),
        "hfs" => "HFS".into(),
        other => other.to_string(),
    }
}

/// `/dev/sdb1` → `/dev/sdb`; `nvme0n1p1` → `nvme0n1`.
fn whole_disk(dev: &Path) -> PathBuf {
    let Some(name) = dev.file_name().and_then(|n| n.to_str()) else {
        return dev.to_path_buf();
    };
    let parent = match name {
        n if n.starts_with("nvme") || n.starts_with("mmcblk") => n
            .rsplit_once('p')
            .filter(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            .map(|(disk, _)| disk.to_string()),
        n if n.starts_with("sd") || n.starts_with("hd") || n.starts_with("vd") => {
            let disk: String = n.chars().take_while(|c| !c.is_ascii_digit()).collect();
            (!disk.is_empty() && disk != n).then_some(disk)
        }
        _ => None,
    };
    match parent {
        Some(disk) => dev.with_file_name(disk),
        None => dev.to_path_buf(),
    }
}

#[cfg(target_os = "linux")]
struct LinuxMount {
    source: PathBuf,
    fstype: String,
    readonly: bool,
}

#[cfg(target_os = "linux")]
fn unescape_mount(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
fn linux_mount_for(mount: &Path) -> Option<LinuxMount> {
    let want = mount.canonicalize().unwrap_or_else(|_| mount.to_path_buf());
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let source = parts.next()?;
        let target = unescape_mount(parts.next()?);
        let fstype = parts.next()?.to_string();
        let opts = parts.next().unwrap_or("");
        let target_path = PathBuf::from(&target);
        let target_canon = target_path.canonicalize().unwrap_or(target_path);
        if target_canon == want {
            return Some(LinuxMount {
                source: PathBuf::from(source),
                fstype,
                readonly: opts.split(',').any(|o| o == "ro"),
            });
        }
    }
    None
}

fn volume_format(mount: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let info = linux_mount_for(mount)?;
        let label = pretty_fstype(&info.fstype);
        Some(if info.readonly {
            format!("{label} (read-only)")
        } else {
            label
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mount;
        None
    }
}

fn firmware_from_block_rev(mount: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let info = linux_mount_for(mount)?;
        if !info.source.starts_with("/dev/") {
            return None;
        }
        let disk = whole_disk(&info.source);
        let name = disk.file_name()?.to_str()?;
        let rev = std::fs::read_to_string(format!("/sys/block/{name}/device/rev")).ok()?;
        let s = rev.trim();
        (!s.is_empty()).then(|| s.to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mount;
        None
    }
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
            let sys = read_sysinfo(&candidate);
            let name = user_assigned_name(&candidate);
            let mut family_id = None;
            let mut serial = sys.serial;
            let mut firmware = sys.firmware;
            if let Some(xml) = read_sysinfo_extended_xml(&candidate) {
                apply_sysinfo_extended_xml(&xml, &mut family_id, &mut serial, &mut firmware);
            }
            if firmware.is_none() {
                firmware = firmware_from_block_rev(&candidate);
            }
            results.push(DetectedIpod {
                mount_point: candidate.clone(),
                model: sys.model,
                serial,
                firmware_version: firmware,
                name,
                family_id,
                gestalt: sys.gestalt,
                sysinfo_empty: sys.empty,
                volume_format: volume_format(&candidate),
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

        let s = read_sysinfo(tmp.path());
        assert_eq!(s.model.as_deref(), Some("MA002"));
        assert_eq!(s.serial.as_deref(), Some("ABC123"));
        assert_eq!(s.firmware.as_deref(), Some("1.3.0"));
        assert!(!s.empty);
    }

    #[test]
    fn test_read_sysinfo_empty_file() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(device_dir.join("SysInfo"), "").unwrap();
        let s = read_sysinfo(tmp.path());
        assert!(s.empty);
        assert!(s.model.is_none());
    }

    #[test]
    fn test_read_sysinfo_gestalt() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(
            device_dir.join("SysInfo"),
            "boardHwSwInterfaceRev: 0x000B0010 (0.0.11 16)\n",
        )
        .unwrap();
        let s = read_sysinfo(tmp.path());
        assert_eq!(s.gestalt, Some(0x000B0010));
        assert!(!s.empty);
    }

    #[test]
    fn test_parse_sysinfo_hex() {
        assert_eq!(parse_sysinfo_hex("0x000B0005 (0.0.11 5)"), Some(0x000B0005));
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
    fn test_user_assigned_name_drops_generic_deviceinfo() {
        let tmp = TempDir::new().unwrap();
        let mount = tmp.path().join("173e8dfc-f647-34c2-a205-dda46088c578");
        let itunes = mount.join("iPod_Control").join("iTunes");
        std::fs::create_dir_all(&itunes).unwrap();
        std::fs::write(itunes.join("DeviceInfo"), utf16le("iPod")).unwrap();
        assert_eq!(user_assigned_name(&mount), None);
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
    fn test_volume_name_skips_automount_uuid() {
        assert_eq!(
            volume_name(Path::new("/run/media/user/dfc-f647-34c2-a205-dda46088c578")),
            None
        );
        assert_eq!(volume_name(Path::new("/media/user/1234-5678")), None);
        assert_eq!(volume_name(Path::new("/media/user/NO NAME")), None);
        assert!(is_generic_ipod_name("dfc-f647-34c2-a205-dda46088c578"));
        assert!(!is_generic_ipod_name("Dublin's iPod"));
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
        assert_eq!(plist_string(xml, "FireWireGUID"), Some("000A2700".into()));
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
        assert_eq!(
            read_sysinfo_extended_xml(tmp.path()).and_then(|xml| plist_integer(&xml, "FamilyID")),
            Some(26)
        );
    }

    #[test]
    fn test_read_family_id_skips_binary_plist() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("iPod_Control").join("Device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(device_dir.join("SysInfoExtended"), b"bplist00\x00\x01").unwrap();
        assert_eq!(
            read_sysinfo_extended_xml(tmp.path()).and_then(|xml| plist_integer(&xml, "FamilyID")),
            None
        );
    }

    #[test]
    fn test_apply_sysinfo_extended_xml_fills_gaps() {
        let xml = r#"<plist><dict>
<key>FireWireGUID</key><string>000A270015CE2062</string>
<key>FamilyID</key><integer>25</integer>
<key>VisibleBuildID</key><string>1.2.3</string>
</dict></plist>"#;
        let mut family_id = None;
        let mut serial = None;
        let mut firmware = None;
        apply_sysinfo_extended_xml(xml, &mut family_id, &mut serial, &mut firmware);
        assert_eq!(family_id, Some(25));
        assert_eq!(serial.as_deref(), Some("000A270015CE2062"));
        assert_eq!(firmware.as_deref(), Some("1.2.3"));
        let mut serial = Some("keep".into());
        apply_sysinfo_extended_xml(xml, &mut family_id, &mut serial, &mut firmware);
        assert_eq!(serial.as_deref(), Some("keep"));
    }

    #[test]
    fn test_pretty_fstype() {
        assert_eq!(pretty_fstype("vfat"), "FAT");
        assert_eq!(pretty_fstype("hfsplus"), "HFS+");
        assert_eq!(pretty_fstype("ext4"), "ext4");
    }

    #[test]
    fn test_whole_disk_strips_partition() {
        assert_eq!(whole_disk(Path::new("/dev/sdb1")), Path::new("/dev/sdb"));
        assert_eq!(whole_disk(Path::new("/dev/sda")), Path::new("/dev/sda"));
        assert_eq!(
            whole_disk(Path::new("/dev/nvme0n1p1")),
            Path::new("/dev/nvme0n1")
        );
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
