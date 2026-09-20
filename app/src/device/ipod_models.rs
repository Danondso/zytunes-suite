//! iPod generation + capacity labels.
//!
//! Identification is composed, not a SKU→pretty-name map:
//!
//! 1. SysInfo `ModelNumStr` suffix (libgpod table; leading `M`/`P` stripped)
//! 2. SysInfoExtended `FamilyID`
//! 3. SysInfo `boardHwSwInterfaceRev` gestalt (1G/2G have no ModelNumStr;
//!    `0x000B0005` = Video 5G, `0x000B0010` = Video 5.5G)
//! 4. USB product ID when SysInfo was never written
//! 5. Empty SysInfo + Video PID `0x1209` → 5.5G (post-2006 firmware leaves
//!    SysInfo at 0 bytes; 5G writes a populated file)
//! 6. Measured storage, snapped to Apple marketing sizes
//!
//! USB vendor inquiry and SCSI SysInfoExtended dumps are not used.

/// Click-wheel / nano / shuffle generation. Touch/iPhone/iPad are omitted:
/// they don't mount `iPod_Control/` as mass storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gen {
    Ipod1G,
    Ipod2G,
    Ipod3G,
    Ipod4G,
    Photo,
    Mini1,
    Mini2,
    Shuffle1,
    Shuffle2,
    Shuffle3,
    Shuffle4,
    Nano1,
    Nano2,
    Nano3,
    Nano4,
    Nano5,
    Nano6,
    Nano7,
    Video,
    Video55,
    Classic,
}

impl Gen {
    fn label(self) -> &'static str {
        match self {
            Gen::Ipod1G => "iPod 1G",
            Gen::Ipod2G => "iPod 2G",
            Gen::Ipod3G => "iPod 3G",
            Gen::Ipod4G => "iPod 4G",
            Gen::Photo => "iPod Photo",
            Gen::Mini1 => "iPod mini",
            Gen::Mini2 => "iPod mini 2G",
            Gen::Shuffle1 => "iPod shuffle",
            Gen::Shuffle2 => "iPod shuffle 2G",
            Gen::Shuffle3 => "iPod shuffle 3G",
            Gen::Shuffle4 => "iPod shuffle 4G",
            Gen::Nano1 => "iPod nano",
            Gen::Nano2 => "iPod nano 2G",
            Gen::Nano3 => "iPod nano 3G",
            Gen::Nano4 => "iPod nano 4G",
            Gen::Nano5 => "iPod nano 5G",
            Gen::Nano6 => "iPod nano 6G",
            Gen::Nano7 => "iPod nano 7G",
            Gen::Video => "iPod Video",
            Gen::Video55 => "iPod Video 5.5G",
            Gen::Classic => "iPod Classic",
        }
    }
}

/// `(suffix, marketing_gb, generation)`. Suffix is ModelNumStr with the
/// leading `M` stripped (`MA446` → `A446`). `gb == 0` means 512 MB.
///
/// Sourced from libgpod `itdb_device.c` `ipod_info_table` (mass-storage
/// models only). Color SKUs share a suffix and collapse to one generation.
const MODELS: &[(&str, u16, Gen)] = &[
    // 1G
    ("8513", 5, Gen::Ipod1G),
    ("8541", 5, Gen::Ipod1G),
    ("8697", 5, Gen::Ipod1G),
    ("8709", 10, Gen::Ipod1G),
    // 2G
    ("8737", 10, Gen::Ipod2G),
    ("8740", 10, Gen::Ipod2G),
    ("8738", 20, Gen::Ipod2G),
    ("8741", 20, Gen::Ipod2G),
    // 3G
    ("8976", 10, Gen::Ipod3G),
    ("8946", 15, Gen::Ipod3G),
    ("9460", 15, Gen::Ipod3G),
    ("9244", 20, Gen::Ipod3G),
    ("8948", 30, Gen::Ipod3G),
    ("9245", 40, Gen::Ipod3G),
    // 4G grayscale
    ("9282", 20, Gen::Ipod4G),
    ("9787", 25, Gen::Ipod4G),
    ("9268", 40, Gen::Ipod4G),
    ("E436", 40, Gen::Ipod4G),
    // mini 1G / 2G
    ("9160", 4, Gen::Mini1),
    ("9436", 4, Gen::Mini1),
    ("9435", 4, Gen::Mini1),
    ("9434", 4, Gen::Mini1),
    ("9437", 4, Gen::Mini1),
    ("9800", 4, Gen::Mini2),
    ("9802", 4, Gen::Mini2),
    ("9804", 4, Gen::Mini2),
    ("9806", 4, Gen::Mini2),
    ("9801", 6, Gen::Mini2),
    ("9803", 6, Gen::Mini2),
    ("9805", 6, Gen::Mini2),
    ("9807", 6, Gen::Mini2),
    // Photo / Color
    ("A079", 20, Gen::Photo),
    ("A127", 20, Gen::Photo),
    ("9829", 30, Gen::Photo),
    ("9585", 40, Gen::Photo),
    ("9830", 60, Gen::Photo),
    ("9586", 60, Gen::Photo),
    ("S492", 30, Gen::Photo),
    // shuffle
    ("9724", 0, Gen::Shuffle1),
    ("9725", 1, Gen::Shuffle1),
    ("A546", 1, Gen::Shuffle2),
    ("A947", 1, Gen::Shuffle2),
    ("A949", 1, Gen::Shuffle2),
    ("A951", 1, Gen::Shuffle2),
    ("A953", 1, Gen::Shuffle2),
    ("C167", 1, Gen::Shuffle2),
    ("B225", 1, Gen::Shuffle2),
    ("B233", 1, Gen::Shuffle2),
    ("B231", 1, Gen::Shuffle2),
    ("B227", 1, Gen::Shuffle2),
    ("B228", 1, Gen::Shuffle2),
    ("B229", 1, Gen::Shuffle2),
    ("B518", 2, Gen::Shuffle2),
    ("B520", 2, Gen::Shuffle2),
    ("B522", 2, Gen::Shuffle2),
    ("B524", 2, Gen::Shuffle2),
    ("B526", 2, Gen::Shuffle2),
    ("C306", 2, Gen::Shuffle3),
    ("C323", 2, Gen::Shuffle3),
    ("C381", 2, Gen::Shuffle3),
    ("C384", 2, Gen::Shuffle3),
    ("C387", 2, Gen::Shuffle3),
    ("B867", 4, Gen::Shuffle3),
    ("C164", 4, Gen::Shuffle3),
    ("C303", 4, Gen::Shuffle3),
    ("C307", 4, Gen::Shuffle3),
    ("C328", 4, Gen::Shuffle3),
    ("C331", 4, Gen::Shuffle3),
    ("C584", 2, Gen::Shuffle4),
    ("C585", 2, Gen::Shuffle4),
    ("C749", 2, Gen::Shuffle4),
    ("C750", 2, Gen::Shuffle4),
    ("C751", 2, Gen::Shuffle4),
    // nano 1–6
    ("A350", 1, Gen::Nano1),
    ("A352", 1, Gen::Nano1),
    ("A004", 2, Gen::Nano1),
    ("A099", 2, Gen::Nano1),
    ("A005", 4, Gen::Nano1),
    ("A107", 4, Gen::Nano1),
    ("A477", 2, Gen::Nano2),
    ("A426", 4, Gen::Nano2),
    ("A428", 4, Gen::Nano2),
    ("A487", 4, Gen::Nano2),
    ("A489", 4, Gen::Nano2),
    ("A725", 4, Gen::Nano2),
    ("A726", 8, Gen::Nano2),
    ("A497", 8, Gen::Nano2),
    ("A978", 4, Gen::Nano3),
    ("A980", 8, Gen::Nano3),
    ("B261", 8, Gen::Nano3),
    ("B249", 8, Gen::Nano3),
    ("B253", 8, Gen::Nano3),
    ("B257", 8, Gen::Nano3),
    ("B480", 4, Gen::Nano4),
    ("B651", 4, Gen::Nano4),
    ("B654", 4, Gen::Nano4),
    ("B657", 4, Gen::Nano4),
    ("B660", 4, Gen::Nano4),
    ("B663", 4, Gen::Nano4),
    ("B666", 4, Gen::Nano4),
    ("B598", 8, Gen::Nano4),
    ("B732", 8, Gen::Nano4),
    ("B735", 8, Gen::Nano4),
    ("B739", 8, Gen::Nano4),
    ("B742", 8, Gen::Nano4),
    ("B745", 8, Gen::Nano4),
    ("B748", 8, Gen::Nano4),
    ("B751", 8, Gen::Nano4),
    ("B754", 8, Gen::Nano4),
    ("B903", 16, Gen::Nano4),
    ("B905", 16, Gen::Nano4),
    ("B907", 16, Gen::Nano4),
    ("B909", 16, Gen::Nano4),
    ("B911", 16, Gen::Nano4),
    ("B913", 16, Gen::Nano4),
    ("B915", 16, Gen::Nano4),
    ("B917", 16, Gen::Nano4),
    ("B918", 16, Gen::Nano4),
    ("C027", 8, Gen::Nano5),
    ("C031", 8, Gen::Nano5),
    ("C034", 8, Gen::Nano5),
    ("C037", 8, Gen::Nano5),
    ("C040", 8, Gen::Nano5),
    ("C043", 8, Gen::Nano5),
    ("C046", 8, Gen::Nano5),
    ("C049", 8, Gen::Nano5),
    ("C050", 8, Gen::Nano5),
    ("C060", 16, Gen::Nano5),
    ("C062", 16, Gen::Nano5),
    ("C064", 16, Gen::Nano5),
    ("C066", 16, Gen::Nano5),
    ("C068", 16, Gen::Nano5),
    ("C070", 16, Gen::Nano5),
    ("C072", 16, Gen::Nano5),
    ("C074", 16, Gen::Nano5),
    ("C075", 16, Gen::Nano5),
    ("C525", 8, Gen::Nano6),
    ("C688", 8, Gen::Nano6),
    ("C689", 8, Gen::Nano6),
    ("C690", 8, Gen::Nano6),
    ("C691", 8, Gen::Nano6),
    ("C692", 8, Gen::Nano6),
    ("C693", 8, Gen::Nano6),
    ("C526", 16, Gen::Nano6),
    ("C694", 16, Gen::Nano6),
    ("C695", 16, Gen::Nano6),
    ("C696", 16, Gen::Nano6),
    ("C697", 16, Gen::Nano6),
    ("C698", 16, Gen::Nano6),
    ("C699", 16, Gen::Nano6),
    // Video 5G / 5.5G
    ("A002", 30, Gen::Video),
    ("A146", 30, Gen::Video),
    ("A003", 60, Gen::Video),
    ("A147", 60, Gen::Video),
    ("A452", 30, Gen::Video),
    ("A444", 30, Gen::Video55),
    ("A446", 30, Gen::Video55),
    ("A664", 30, Gen::Video55),
    ("A448", 80, Gen::Video55),
    ("A450", 80, Gen::Video55),
    // Classic 6G / 6.5G / 7G
    ("B029", 80, Gen::Classic),
    ("B147", 80, Gen::Classic),
    ("B145", 160, Gen::Classic),
    ("B148", 160, Gen::Classic),
    ("B150", 160, Gen::Classic),
    ("B562", 120, Gen::Classic),
    ("B565", 120, Gen::Classic),
    ("C293", 160, Gen::Classic),
    ("C297", 160, Gen::Classic),
];

/// SysInfoExtended FamilyID → generation. Conservative set of IDs that
/// show up consistently in libgpod / ipodlinux notes. Disputed nano IDs
/// are omitted; ModelNumStr covers those when SysInfo is populated.
const FAMILY_IDS: &[(u32, Gen)] = &[
    (1, Gen::Ipod1G),
    (2, Gen::Ipod2G),
    (3, Gen::Ipod3G),
    (4, Gen::Mini1),
    (5, Gen::Ipod4G),
    (6, Gen::Photo),
    (7, Gen::Mini2),
    (19, Gen::Video),
    (25, Gen::Video55),
    (26, Gen::Video55),
    (31, Gen::Classic),
    (34, Gen::Classic),
    (37, Gen::Classic),
];

/// Apple VID `0x05AC` mass-storage product IDs (not DFU/WTF). 5G and 5.5G
/// Video share `0x1209`.
const USB_PIDS: &[(u16, Gen)] = &[
    (0x1201, Gen::Ipod3G),
    (0x1202, Gen::Ipod2G),
    (0x1203, Gen::Ipod4G),
    (0x1204, Gen::Photo),
    (0x1205, Gen::Mini1),
    (0x1209, Gen::Video),
    (0x120A, Gen::Nano1),
    (0x1260, Gen::Nano2),
    (0x1261, Gen::Classic),
    (0x1262, Gen::Nano3),
    (0x1263, Gen::Nano4),
    (0x1265, Gen::Nano5),
    (0x1266, Gen::Nano6),
    (0x1267, Gen::Nano7),
    (0x1300, Gen::Shuffle1),
    (0x1301, Gen::Shuffle2),
    (0x1302, Gen::Shuffle3),
    (0x1303, Gen::Shuffle4),
];

/// Marketing capacities Apple actually shipped. Used when we only have a
/// byte count (no ModelNumStr).
const MARKETING_GB: &[u16] = &[
    1, 2, 4, 5, 8, 10, 15, 16, 20, 30, 32, 40, 60, 64, 80, 120, 160,
];

/// Hints gathered from SysInfo, SysInfoExtended, USB, and storage.
/// Generation and capacity are composed independently — see [`ipod_model_label`].
#[derive(Clone, Copy, Debug, Default)]
pub struct IpodModelHints<'a> {
    pub model_num: Option<&'a str>,
    pub family_id: Option<u32>,
    pub usb_pid: Option<u16>,
    pub gestalt: Option<u32>,
    /// Firmware created a 0-byte SysInfo (post-2006 Video 5.5G / nano 2G+).
    pub sysinfo_empty: bool,
    pub total_bytes: Option<u64>,
}

/// Identify an iPod from [`IpodModelHints`].
pub fn ipod_model_label(hints: IpodModelHints<'_>) -> String {
    let from_suffix = hints.model_num.and_then(lookup_model_num);
    let gb = from_suffix
        .map(|(_, gb)| gb)
        .or_else(|| hints.total_bytes.map(snap_gb));
    let gen = from_suffix
        .map(|(g, _)| g)
        .or_else(|| hints.family_id.and_then(lookup_family))
        .or_else(|| hints.gestalt.and_then(lookup_gestalt))
        .or_else(|| hints.usb_pid.and_then(lookup_usb_pid));
    let gen = refine_video_generation(gen, gb, hints);
    match (gen, gb) {
        (Some(g), Some(n)) => format!("{} {}", g.label(), format_gb(n)),
        (Some(g), None) => g.label().to_string(),
        (None, Some(n)) => format!("iPod {}", format_gb(n)),
        (None, None) => "iPod".into(),
    }
}

/// True for Apple mass-storage iPod product IDs we can map to a generation.
pub(crate) fn ipod_usb_pid_known(pid: u16) -> bool {
    lookup_usb_pid(pid).is_some()
}

fn lookup_model_num(raw: &str) -> Option<(Gen, u16)> {
    let suffix = model_suffix(raw);
    MODELS
        .iter()
        .find(|(s, _, _)| *s == suffix)
        .map(|&(_, gb, gen)| (gen, gb))
}

fn lookup_family(id: u32) -> Option<Gen> {
    FAMILY_IDS
        .iter()
        .find(|(fid, _)| *fid == id)
        .map(|(_, g)| *g)
}

fn lookup_usb_pid(pid: u16) -> Option<Gen> {
    USB_PIDS.iter().find(|(p, _)| *p == pid).map(|(_, g)| *g)
}

/// `boardHwSwInterfaceRev` gestalt. Exact values for Video 5G/5.5G; high
/// 16 bits for 1G/2G (those SKUs have no ModelNumStr).
fn lookup_gestalt(g: u32) -> Option<Gen> {
    match g {
        0x000B0005 => Some(Gen::Video),
        0x000B0010 => Some(Gen::Video55),
        _ => match g >> 16 {
            1 => Some(Gen::Ipod1G),
            2 => Some(Gen::Ipod2G),
            _ => None,
        },
    }
}

/// 5G Video shipped 30/60GB; 5.5G shipped 30/80GB. PID `0x1209` is shared.
/// 80GB promotes to 5.5G. An empty SysInfo on a Video PID is the post-2006
/// firmware behaviour (5.5G), not 5G (which writes a populated SysInfo).
fn refine_video_generation(
    gen: Option<Gen>,
    gb: Option<u16>,
    hints: IpodModelHints<'_>,
) -> Option<Gen> {
    match (gen, gb) {
        (Some(Gen::Video), Some(80)) => Some(Gen::Video55),
        (Some(Gen::Video), _) if hints.sysinfo_empty && hints.usb_pid == Some(0x1209) => {
            Some(Gen::Video55)
        }
        (g, _) => g,
    }
}

/// libgpod: if the first character is a letter, skip it (`MA446` → `A446`).
/// Also drop regional `LL/A` suffixes.
fn model_suffix(raw: &str) -> String {
    let s = raw.trim();
    let s = s.split(['/', ' ', '-']).next().unwrap_or(s);
    let s = s
        .trim_end_matches("LL")
        .trim_end_matches("ll")
        .to_ascii_uppercase();
    if s.len() > 1 && matches!(s.as_bytes()[0], b'M' | b'P') {
        s[1..].to_string()
    } else {
        s
    }
}

fn snap_gb(total_bytes: u64) -> u16 {
    if total_bytes < 750_000_000 {
        return 0;
    }
    let gb = total_bytes as f64 / 1_000_000_000.0;
    let measured = gb.round().clamp(1.0, u16::MAX as f64) as u16;
    let snapped = MARKETING_GB.iter().copied().min_by(|&a, &b| {
        (a as f64 - gb)
            .abs()
            .partial_cmp(&(b as f64 - gb).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    match snapped {
        Some(n) if (n as f64 - gb).abs() / gb <= 0.15 => n,
        _ => measured,
    }
}

fn format_gb(gb: u16) -> String {
    if gb == 0 {
        "512MB".into()
    } else {
        format!("{gb}GB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_strips_m_and_regional_tail() {
        assert_eq!(model_suffix("MA446LL/A"), "A446");
        assert_eq!(model_suffix("MB147"), "B147");
        assert_eq!(model_suffix("MC297LL"), "C297");
        assert_eq!(model_suffix("A446"), "A446");
        assert_eq!(model_suffix("8513"), "8513");
        assert_eq!(model_suffix("PA003"), "A003", "HP SKUs use a P prefix");
    }

    fn label(hints: IpodModelHints<'_>) -> String {
        ipod_model_label(hints)
    }

    #[test]
    fn known_suffix_composes_generation_and_factory_gb() {
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA446LL/A"),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MB147"),
                ..Default::default()
            }),
            "iPod Classic 80GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MC297LL"),
                ..Default::default()
            }),
            "iPod Classic 160GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA444"),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA004"),
                ..Default::default()
            }),
            "iPod nano 2GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("M9724"),
                ..Default::default()
            }),
            "iPod shuffle 512MB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MC525"),
                ..Default::default()
            }),
            "iPod nano 6G 8GB"
        );
    }

    #[test]
    fn unknown_suffix_does_not_guess_classic() {
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("XX999"),
                ..Default::default()
            }),
            "iPod"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("ZZ000"),
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod 80GB"
        );
    }

    #[test]
    fn family_id_fills_generation_when_suffix_unknown() {
        assert_eq!(
            label(IpodModelHints {
                family_id: Some(31),
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod Classic 80GB"
        );
        assert_eq!(
            label(IpodModelHints {
                family_id: Some(19),
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                family_id: Some(25),
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA446"),
                family_id: Some(31),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
    }

    #[test]
    fn usb_pid_fills_generation_when_sysinfo_empty() {
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1209),
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1209),
                sysinfo_empty: true,
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1209),
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 80GB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1261),
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod Classic 80GB"
        );
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA446"),
                usb_pid: Some(0x1261),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
    }

    #[test]
    fn gestalt_fills_generation() {
        assert_eq!(
            label(IpodModelHints {
                gestalt: Some(0x000B0005),
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                gestalt: Some(0x000B0010),
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                gestalt: Some(0x00010000),
                total_bytes: Some(5_000_000_000),
                ..Default::default()
            }),
            "iPod 1G 5GB"
        );
    }

    #[test]
    fn storage_only_snaps_to_marketing_size() {
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(28_000_000_000),
                ..Default::default()
            }),
            "iPod 30GB"
        );
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(55_000_000_000),
                ..Default::default()
            }),
            "iPod 60GB"
        );
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod 80GB"
        );
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(111_000_000_000),
                ..Default::default()
            }),
            "iPod 120GB"
        );
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(149_000_000_000),
                ..Default::default()
            }),
            "iPod 160GB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1209),
                sysinfo_empty: true,
                total_bytes: Some(256_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 256GB"
        );
        assert_eq!(
            label(IpodModelHints {
                total_bytes: Some(240_000_000_000),
                ..Default::default()
            }),
            "iPod 240GB"
        );
    }

    #[test]
    fn shuffle_usb_pid_fills_generation() {
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1300),
                total_bytes: Some(512_000_000),
                ..Default::default()
            }),
            "iPod shuffle 512MB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1301),
                total_bytes: Some(1_000_000_000),
                ..Default::default()
            }),
            "iPod shuffle 2G 1GB"
        );
        assert_eq!(
            label(IpodModelHints {
                usb_pid: Some(0x1303),
                total_bytes: Some(2_000_000_000),
                ..Default::default()
            }),
            "iPod shuffle 4G 2GB"
        );
    }

    #[test]
    fn known_suffix_keeps_factory_gb_even_if_storage_differs() {
        assert_eq!(
            label(IpodModelHints {
                model_num: Some("MA446"),
                total_bytes: Some(74_000_000_000),
                ..Default::default()
            }),
            "iPod Video 5.5G 30GB"
        );
    }
}
