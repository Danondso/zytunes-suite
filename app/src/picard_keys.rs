//! Names for the MusicBrainz / AcoustID fields that lofty has no
//! `ItemKey` for, spelled the way Picard writes them in each container.
//!
//! Picard's canonical (Vorbis Comment) names are uppercase with
//! underscores; in ID3v2 the same facts live in `TXXX` frames with spaced
//! names ("MusicBrainz Album Type"), and in MP4 as freeform iTunes atoms
//! whose full key carries a `----:com.apple.iTunes:` prefix. Writing the
//! Vorbis spelling everywhere left MP3s with a second, differently named
//! copy beside Picard's and dropped the value outright on MP4 (lofty's
//! writer only accepts the prefixed form), and reading only the Vorbis
//! spelling never matched what Picard-tagged files actually hold — so the
//! rows came back as changes on every open.

use lofty::tag::TagType;

/// A field stored under a format-specific "unknown" key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicardField {
    AlbumType,
    AlbumStatus,
    AlbumPackaging,
    ReleaseCountry,
    AcoustidFingerprint,
    AcoustidId,
}

/// MP4 freeform atoms are keyed `----:<mean>:<name>`; the name is the
/// last segment.
pub fn strip_freeform_prefix(key: &str) -> &str {
    match key.strip_prefix("----:") {
        Some(rest) => rest.rsplit_once(':').map_or(rest, |(_, name)| name),
        None => key,
    }
}

impl PicardField {
    pub const ALL: [PicardField; 6] = [
        PicardField::AlbumType,
        PicardField::AlbumStatus,
        PicardField::AlbumPackaging,
        PicardField::ReleaseCountry,
        PicardField::AcoustidFingerprint,
        PicardField::AcoustidId,
    ];

    /// The Vorbis spelling, also our internal name.
    pub fn canonical(self) -> &'static str {
        match self {
            PicardField::AlbumType => "MUSICBRAINZ_ALBUMTYPE",
            PicardField::AlbumStatus => "MUSICBRAINZ_ALBUMSTATUS",
            PicardField::AlbumPackaging => "MUSICBRAINZ_ALBUMPACKAGING",
            PicardField::ReleaseCountry => "RELEASECOUNTRY",
            PicardField::AcoustidFingerprint => "ACOUSTID_FINGERPRINT",
            PicardField::AcoustidId => "ACOUSTID_ID",
        }
    }

    /// Picard's spaced spelling (ID3v2 `TXXX` description, MP4 atom name).
    fn spaced(self) -> &'static str {
        match self {
            PicardField::AlbumType => "MusicBrainz Album Type",
            PicardField::AlbumStatus => "MusicBrainz Album Status",
            PicardField::AlbumPackaging => "MusicBrainz Album Packaging",
            PicardField::ReleaseCountry => "MusicBrainz Album Release Country",
            PicardField::AcoustidFingerprint => "Acoustid Fingerprint",
            PicardField::AcoustidId => "Acoustid Id",
        }
    }

    pub fn from_canonical(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.canonical().eq_ignore_ascii_case(name))
    }

    /// The key to write for `tag_type`.
    pub fn key_for(self, tag_type: TagType) -> String {
        match tag_type {
            TagType::Id3v2 => self.spaced().to_string(),
            TagType::Mp4Ilst => format!("----:com.apple.iTunes:{}", self.spaced()),
            _ => self.canonical().to_string(),
        }
    }

    /// `key` (as lofty reports it, prefix and all) names this field in
    /// any of its spellings.
    pub fn matches(self, key: &str) -> bool {
        let name = strip_freeform_prefix(key);
        name.eq_ignore_ascii_case(self.canonical()) || name.eq_ignore_ascii_case(self.spaced())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_picard_per_container() {
        assert_eq!(
            PicardField::AlbumType.key_for(TagType::VorbisComments),
            "MUSICBRAINZ_ALBUMTYPE"
        );
        assert_eq!(
            PicardField::AlbumType.key_for(TagType::Id3v2),
            "MusicBrainz Album Type"
        );
        assert_eq!(
            PicardField::ReleaseCountry.key_for(TagType::Mp4Ilst),
            "----:com.apple.iTunes:MusicBrainz Album Release Country"
        );
        assert_eq!(
            PicardField::AcoustidFingerprint.key_for(TagType::Mp4Ilst),
            "----:com.apple.iTunes:Acoustid Fingerprint"
        );
        assert_eq!(
            PicardField::AcoustidId.key_for(TagType::VorbisComments),
            "ACOUSTID_ID"
        );
        assert_eq!(
            PicardField::AcoustidId.key_for(TagType::Id3v2),
            "Acoustid Id"
        );
        assert_eq!(
            PicardField::AcoustidId.key_for(TagType::Mp4Ilst),
            "----:com.apple.iTunes:Acoustid Id"
        );
    }

    #[test]
    fn matches_every_spelling_lofty_reports() {
        let f = PicardField::AlbumStatus;
        assert!(f.matches("MUSICBRAINZ_ALBUMSTATUS"));
        assert!(f.matches("musicbrainz_albumstatus"));
        assert!(f.matches("MusicBrainz Album Status"));
        assert!(f.matches("----:com.apple.iTunes:MusicBrainz Album Status"));
        assert!(!f.matches("MusicBrainz Album Type"));
        assert!(!PicardField::AlbumType.matches("----:com.apple.iTunes:ALBUMARTISTS"));
    }

    #[test]
    fn freeform_prefix_is_stripped_only_when_present() {
        assert_eq!(
            strip_freeform_prefix("----:com.apple.iTunes:Acoustid Id"),
            "Acoustid Id"
        );
        assert_eq!(strip_freeform_prefix("SCRIPT"), "SCRIPT");
        assert_eq!(
            PicardField::from_canonical("releasecountry"),
            Some(PicardField::ReleaseCountry)
        );
    }
}
