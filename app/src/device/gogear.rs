//! Philips GoGear mass-storage backend.
//!
//! The ViBE (and most flash GoGears) enumerate as USB MSC — no MTP, no
//! iTunesDB. Detection keys on Philips VID `0x0471` plus a known product
//! ID / "GoGear" product string. The volume must be mounted for a session;
//! USB identity still surfaces when the disk is present but unmounted so
//! the TUI can say so instead of "No devices detected".

use std::path::{Path, PathBuf};

use rusb::UsbContext;

use super::{DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily};
use crate::mtp::DeviceSession;

const PHILIPS_VID: u16 = 0x0471;

/// MSC GoGears we can name. Unknown Philips PIDs still match if the USB
/// product string contains "GoGear".
const GOGEAR_PIDS: &[(u16, &str)] = &[
    (0x20b6, "GoGear ViBE"),
    (0x20e5, "GoGear ViBE"),
    (0x2168, "GoGear"),
    (0x0828, "GoGear"),
    (0x084e, "GoGear"),
    (0x1103, "GoGear"),
];

pub struct GogearDeviceData {
    pub mount_point: Option<PathBuf>,
    pub usb_pid: Option<u16>,
    pub volume_format: Option<String>,
}

pub struct GogearBackend;

impl DeviceBackend for GogearBackend {
    fn detect(&self) -> Result<DetectedDevice, String> {
        let usb = find_usb_gogear();
        let env_mount = std::env::var("GOGEAR_MOUNT_PATH")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_dir());

        let (mount, volume_format) = if let Some(p) = env_mount {
            let fmt = fstype_for_mount(&p);
            (Some(p), fmt)
        } else {
            linux_mount_for_usb(usb.as_ref())
                .or_else(scan_volumes)
                .map(|(p, fmt)| (Some(p), fmt))
                .unwrap_or((None, None))
        };

        let Some(usb) = usb else {
            if mount.is_some() {
                return Ok(detected(
                    "GoGear",
                    None,
                    None,
                    None,
                    GogearDeviceData {
                        mount_point: mount,
                        usb_pid: None,
                        volume_format,
                    },
                ));
            }
            return Err("No GoGear detected".into());
        };

        let name = usb
            .product
            .clone()
            .or_else(|| model_from_pid(usb.pid).map(str::to_string))
            .unwrap_or_else(|| "GoGear".into());

        Ok(detected(
            &name,
            model_from_pid(usb.pid)
                .map(str::to_string)
                .or(usb.product.clone()),
            usb.serial.clone(),
            usb.firmware.clone(),
            GogearDeviceData {
                mount_point: mount,
                usb_pid: Some(usb.pid),
                volume_format,
            },
        ))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Gogear,
            // ViBE manual: MP3, WMA, PCM. Other flash GoGears are the same
            // drag-and-drop set. No lossless container.
            supported_formats: &["mp3", "wma", "wav"],
            transcode_target: "mp3",
            lossless_target: None,
            music_root: "/MUSIC",
            max_art_dimensions: None,
        }
    }

    fn open_session(
        &self,
        detected: &DetectedDevice,
        log: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Box<dyn DeviceSession + Send>, String> {
        let data = detected
            .backend_data
            .downcast_ref::<GogearDeviceData>()
            .ok_or_else(|| "Invalid backend data for GoGear".to_string())?;
        let mount = data.mount_point.clone().ok_or_else(|| {
            format!(
                "{} found but not mounted — mount the USB volume, then press c",
                detected.name
            )
        })?;
        if let Some(ref tx) = log {
            let _ = tx.send(format!("Opening GoGear volume {}", mount.display()));
        }
        Ok(Box::new(crate::mtp::GogearSession::new(mount)))
    }
}

struct UsbGogear {
    pid: u16,
    product: Option<String>,
    serial: Option<String>,
    firmware: Option<String>,
}

fn detected(
    name: &str,
    model: Option<String>,
    serial: Option<String>,
    firmware: Option<String>,
    data: GogearDeviceData,
) -> DetectedDevice {
    DetectedDevice {
        family: DeviceFamily::Gogear,
        name: name.to_string(),
        model,
        serial,
        firmware,
        backend_data: Box::new(data),
    }
}

fn model_from_pid(pid: u16) -> Option<&'static str> {
    GOGEAR_PIDS.iter().find(|(p, _)| *p == pid).map(|(_, n)| *n)
}

fn is_gogear_pid(pid: u16) -> bool {
    GOGEAR_PIDS.iter().any(|(p, _)| *p == pid)
}

fn looks_like_gogear_product(s: &str) -> bool {
    s.to_ascii_lowercase().contains("gogear")
}

fn find_usb_gogear() -> Option<UsbGogear> {
    #[cfg(target_os = "linux")]
    {
        if let Some(u) = find_usb_sysfs() {
            return Some(u);
        }
    }
    find_usb_rusb()
}

#[cfg(target_os = "linux")]
fn read_sysfs_hex_u16(path: &Path) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    u16::from_str_radix(s.trim(), 16).ok()
}

#[cfg(target_os = "linux")]
fn find_usb_sysfs() -> Option<UsbGogear> {
    let root = Path::new("/sys/bus/usb/devices");
    for ent in std::fs::read_dir(root).ok()? {
        let path = ent.ok()?.path();
        let Some(vid) = read_sysfs_hex_u16(&path.join("idVendor")) else {
            continue;
        };
        if vid != PHILIPS_VID {
            continue;
        }
        let Some(pid) = read_sysfs_hex_u16(&path.join("idProduct")) else {
            continue;
        };
        let product = std::fs::read_to_string(path.join("product"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if !is_gogear_pid(pid) && !product.as_deref().is_some_and(looks_like_gogear_product) {
            continue;
        }
        let serial = std::fs::read_to_string(path.join("serial"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let firmware = firmware_from_usb_sysfs(&path);
        return Some(UsbGogear {
            pid,
            product,
            serial,
            firmware,
        });
    }
    None
}

#[cfg(target_os = "linux")]
fn firmware_from_usb_sysfs(usb_dev: &Path) -> Option<String> {
    // Walk block devices whose sysfs path sits under this USB device.
    let usb_canon = usb_dev.canonicalize().ok()?;
    for ent in std::fs::read_dir("/sys/block").ok()? {
        let Ok(ent) = ent else {
            continue;
        };
        let block = ent.path();
        let Ok(start) = block.join("device").canonicalize() else {
            continue;
        };
        for ancestor in start.ancestors() {
            if ancestor == usb_canon {
                if let Ok(rev) = std::fs::read_to_string(block.join("device").join("rev")) {
                    let s = rev.trim();
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
                break;
            }
        }
    }
    None
}

fn find_usb_rusb() -> Option<UsbGogear> {
    let ctx = rusb::Context::new().ok()?;
    for dev in ctx.devices().ok()?.iter() {
        let desc = dev.device_descriptor().ok()?;
        if desc.vendor_id() != PHILIPS_VID {
            continue;
        }
        let pid = desc.product_id();
        let handle = dev.open().ok();
        let product = handle.as_ref().and_then(|h| {
            h.read_product_string_ascii(&desc)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        if !is_gogear_pid(pid) && !product.as_deref().is_some_and(looks_like_gogear_product) {
            continue;
        }
        let serial = handle.and_then(|h| {
            h.read_serial_number_string_ascii(&desc)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        return Some(UsbGogear {
            pid,
            product,
            serial,
            firmware: None,
        });
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_mount_for_usb(usb: Option<&UsbGogear>) -> Option<(PathBuf, Option<String>)> {
    for ent in std::fs::read_dir("/sys/block").ok()? {
        let Ok(ent) = ent else {
            continue;
        };
        let block = ent.path();
        let Some(name) = block
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // zram/loop/nvme entries often have no `device` symlink. `?` here
        // used to abort the whole scan before we reached the GoGear disk.
        let Ok(start) = block.join("device").canonicalize() else {
            continue;
        };
        let mut matched = false;
        for ancestor in start.ancestors() {
            let Some(vid) = read_sysfs_hex_u16(&ancestor.join("idVendor")) else {
                continue;
            };
            if vid != PHILIPS_VID {
                continue;
            }
            let Some(pid) = read_sysfs_hex_u16(&ancestor.join("idProduct")) else {
                continue;
            };
            let product = std::fs::read_to_string(ancestor.join("product"))
                .ok()
                .map(|s| s.trim().to_string());
            let pid_ok =
                is_gogear_pid(pid) || product.as_deref().is_some_and(looks_like_gogear_product);
            if !pid_ok {
                continue;
            }
            if usb.is_some_and(|u| u.pid != pid) {
                continue;
            }
            matched = true;
            break;
        }
        if !matched {
            continue;
        }
        if let Some(found) = mount_for_disk(&name) {
            return Some(found);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn linux_mount_for_usb(_usb: Option<&UsbGogear>) -> Option<(PathBuf, Option<String>)> {
    None
}

fn mount_for_disk(disk: &str) -> Option<(PathBuf, Option<String>)> {
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    let prefix = format!("/dev/{disk}");
    let mut best = None;
    for line in mounts.lines() {
        let mut cols = line.split_whitespace();
        let src = cols.next()?;
        let dest = cols.next()?;
        let fstype = cols.next()?;
        let belongs = src == prefix
            || src
                .strip_prefix(&prefix)
                .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()));
        if belongs {
            best = Some((PathBuf::from(dest), Some(pretty_fstype(fstype))));
        }
    }
    best
}

fn pretty_fstype(fstype: &str) -> String {
    match fstype {
        "vfat" | "msdos" | "fat" | "exfat" => "FAT".into(),
        other => other.to_uppercase(),
    }
}

fn fstype_for_mount(mount: &Path) -> Option<String> {
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    let want = mount.canonicalize().unwrap_or_else(|_| mount.to_path_buf());
    for line in mounts.lines() {
        let mut cols = line.split_whitespace();
        let _src = cols.next()?;
        let dest = cols.next()?;
        let fstype = cols.next()?;
        let dest_p = PathBuf::from(dest);
        if dest_p.canonicalize().unwrap_or(dest_p) == want {
            return Some(pretty_fstype(fstype));
        }
    }
    None
}

fn dir_ci(root: &Path, name: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.file_name().eq_ignore_ascii_case(name))
}

/// ViBE (and most flash GoGears): `_system/`, or `Music/` plus the on-disk
/// song index (`STDBDATA.DAT` / `DevDiversity.ini` / `CMI_TITLE.IDX`).
fn looks_like_gogear_volume(p: &Path) -> bool {
    if p.join("iPod_Control").is_dir() {
        return false;
    }
    if dir_ci(p, "_system") {
        return true;
    }
    if !dir_ci(p, "Music") {
        return false;
    }
    ["STDBDATA.DAT", "DevDiversity.ini", "CMI_TITLE.IDX"]
        .iter()
        .any(|n| dir_ci(p, n))
}

/// Fallback: mounted volumes that look like a Philips player. Used when
/// USB identity isn't available (`GOGEAR_MOUNT_PATH`, or a desktop
/// automount we haven't linked yet).
fn scan_volumes() -> Option<(PathBuf, Option<String>)> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut candidates = Vec::new();
    for root in [
        PathBuf::from("/media").join(&user),
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/mnt"),
        PathBuf::from("/Volumes"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        }
    }
    for p in candidates {
        if looks_like_gogear_volume(&p) {
            let fmt = fstype_for_mount(&p);
            return Some((p, fmt));
        }
    }
    None
}

/// Marketing capacity for ViBE SKUs (2/4/8/16 GB NAND). Used after we
/// know total bytes from `df`.
pub fn gogear_capacity_label(model: &str, total_bytes: u64) -> String {
    const SKUS: &[(u64, &str)] = &[
        (2 * 1024 * 1024 * 1024, "2GB"),
        (4 * 1024 * 1024 * 1024, "4GB"),
        (8 * 1024 * 1024 * 1024, "8GB"),
        (16 * 1024 * 1024 * 1024, "16GB"),
    ];
    for &(sku, tag) in SKUS {
        // 20%: ViBE 4GB NAND reports ~3.6 GB usable (~16% under 4 GiB).
        if total_bytes.abs_diff(sku) * 100 / sku <= 20 {
            return format!("{model} {tag}");
        }
    }
    format!("{model} {}GB", total_bytes / (1024 * 1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vibe_pid_is_named() {
        assert_eq!(model_from_pid(0x20b6), Some("GoGear ViBE"));
        assert!(is_gogear_pid(0x20b6));
        assert!(!is_gogear_pid(0x0001));
        assert!(looks_like_gogear_product("GoGear ViBE  "));
        assert!(!looks_like_gogear_product("Philips TV"));
    }

    #[test]
    fn vibe_4gb_snaps_to_marketing_size() {
        let nand = 3_600_000_000u64;
        assert_eq!(
            gogear_capacity_label("GoGear ViBE", nand),
            "GoGear ViBE 4GB"
        );
    }

    #[test]
    fn capabilities_are_lossy_msc() {
        let caps = GogearBackend.capabilities();
        assert_eq!(caps.family, DeviceFamily::Gogear);
        assert_eq!(caps.transcode_target, "mp3");
        assert!(caps.lossless_target.is_none());
        assert!(caps.supported_formats.contains(&"mp3"));
        assert!(caps.supported_formats.contains(&"wma"));
        assert_eq!(caps.music_root, "/MUSIC");
    }

    #[test]
    fn disk_source_match_does_not_eat_sdda() {
        // mount_for_disk lives on /proc/mounts; the prefix rule is the
        // bit we can unit-test without a live disk.
        let prefix = "/dev/sdd";
        let belongs = |src: &str| {
            src == prefix
                || src
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
        };
        assert!(belongs("/dev/sdd"));
        assert!(belongs("/dev/sdd1"));
        assert!(!belongs("/dev/sdda"));
        assert!(!belongs("/dev/sde"));
    }

    #[test]
    fn vibe_layout_without_system_dir_is_still_a_gogear() {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-gogear-vol-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("Music")).unwrap();
        std::fs::write(dir.join("STDBDATA.DAT"), b"").unwrap();
        assert!(looks_like_gogear_volume(&dir));
        assert!(!looks_like_gogear_volume(&dir.join("Music")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
