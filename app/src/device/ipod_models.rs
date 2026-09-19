//! iPod generation + capacity labels.
//!
//! Identification is composed, not a SKU→pretty-name map:
//!
//! 1. SysInfo `ModelNumStr` suffix (libgpod's `ipod_info_table` — strip the
//!    leading `M` so `MA446LL/A` and `A446` both hit) → generation + factory GB
//! 2. SysInfoExtended `FamilyID` → generation only (covers post-2006 devices
//!    whose SysInfo is empty)
//! 3. Measured storage, snapped to Apple marketing sizes → capacity
//!
//! Unknown part numbers still get a generation from FamilyID, or at worst
//! `"iPod 80GB"` from capacity — never a guessed "Classic".

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
    (26, Gen::Video55),
    (31, Gen::Classic),
    (34, Gen::Classic),
    (37, Gen::Classic),
];

/// Marketing capacities Apple actually shipped. Used when we only have a
/// byte count (no ModelNumStr).
const MARKETING_GB: &[u16] = &[
    1, 2, 4, 5, 8, 10, 15, 16, 20, 30, 32, 40, 60, 64, 80, 120, 160,
];

/// Identify an iPod from SysInfo `ModelNumStr`, optional SysInfoExtended
/// `FamilyID`, and optional total storage. Mirrors the Zune storage→name
/// helper, but generation and capacity are composed independently.
pub fn ipod_model_label(
    model_num: Option<&str>,
    family_id: Option<u32>,
    total_bytes: Option<u64>,
) -> String {
    let from_suffix = model_num.and_then(lookup_model_num);
    let gen = from_suffix
        .map(|(g, _)| g)
        .or_else(|| family_id.and_then(lookup_family));
    let gb = from_suffix
        .map(|(_, gb)| gb)
        .or_else(|| total_bytes.map(snap_gb));
    match (gen, gb) {
        (Some(g), Some(n)) => format!("{} {}", g.label(), format_gb(n)),
        (Some(g), None) => g.label().to_string(),
        (None, Some(n)) => format!("iPod {}", format_gb(n)),
        (None, None) => "iPod".into(),
    }
}

/// Capacity-only label when generation is unknown. Does **not** assume Classic.
pub fn ipod_model_from_storage(total_bytes: u64) -> String {
    format!("iPod {}", format_gb(snap_gb(total_bytes)))
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

/// libgpod: if the first character is a letter, skip it (`MA446` → `A446`).
/// Also drop regional `LL/A` suffixes.
fn model_suffix(raw: &str) -> String {
    let s = raw.trim();
    let s = s.split(['/', ' ', '-']).next().unwrap_or(s);
    let s = s
        .trim_end_matches("LL")
        .trim_end_matches("ll")
        .to_ascii_uppercase();
    if s.starts_with('M') && s.len() > 1 {
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
    MARKETING_GB
        .iter()
        .copied()
        .min_by(|&a, &b| {
            (a as f64 - gb)
                .abs()
                .partial_cmp(&(b as f64 - gb).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(gb.round() as u16)
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
    }

    #[test]
    fn known_suffix_composes_generation_and_factory_gb() {
        assert_eq!(
            ipod_model_label(Some("MA446LL/A"), None, None),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(
            ipod_model_label(Some("MB147"), None, None),
            "iPod Classic 80GB"
        );
        assert_eq!(
            ipod_model_label(Some("MC297LL"), None, None),
            "iPod Classic 160GB"
        );
        // Previously missing from the hand-written SKU list.
        assert_eq!(
            ipod_model_label(Some("MA444"), None, None),
            "iPod Video 5.5G 30GB"
        );
        assert_eq!(ipod_model_label(Some("MA004"), None, None), "iPod nano 2GB");
        assert_eq!(
            ipod_model_label(Some("M9724"), None, None),
            "iPod shuffle 512MB"
        );
        assert_eq!(
            ipod_model_label(Some("MC525"), None, None),
            "iPod nano 6G 8GB"
        );
    }

    #[test]
    fn unknown_suffix_does_not_guess_classic() {
        assert_eq!(ipod_model_label(Some("XX999"), None, None), "iPod");
        assert_eq!(
            ipod_model_label(Some("ZZ000"), None, Some(74_000_000_000)),
            "iPod 80GB"
        );
    }

    #[test]
    fn family_id_fills_generation_when_suffix_unknown() {
        assert_eq!(
            ipod_model_label(None, Some(31), Some(74_000_000_000)),
            "iPod Classic 80GB"
        );
        assert_eq!(
            ipod_model_label(None, Some(19), Some(28_000_000_000)),
            "iPod Video 30GB"
        );
        // Suffix still wins over FamilyID.
        assert_eq!(
            ipod_model_label(Some("MA446"), Some(31), None),
            "iPod Video 5.5G 30GB"
        );
    }

    #[test]
    fn storage_only_snaps_to_marketing_size() {
        assert_eq!(ipod_model_from_storage(28_000_000_000), "iPod 30GB");
        assert_eq!(ipod_model_from_storage(55_000_000_000), "iPod 60GB");
        assert_eq!(ipod_model_from_storage(74_000_000_000), "iPod 80GB");
        assert_eq!(ipod_model_from_storage(111_000_000_000), "iPod 120GB");
        assert_eq!(ipod_model_from_storage(149_000_000_000), "iPod 160GB");
    }

    #[test]
    fn known_suffix_keeps_factory_gb_even_if_storage_differs() {
        // Flash-modded 30GB Video still identifies as the factory SKU;
        // the storage bar shows the real df figure separately.
        assert_eq!(
            ipod_model_label(Some("MA446"), None, Some(74_000_000_000)),
            "iPod Video 5.5G 30GB"
        );
    }
}
