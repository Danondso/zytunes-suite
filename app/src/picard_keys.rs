//! Names for the MusicBrainz / AcoustID fields that lofty has no
//! `ItemKey` for, spelled the way Picard writes them in each container.
//!
//! Picard spells each field per container: Vorbis comments use its
//! internal tag name uppercased (`RELEASETYPE`), APE keeps the older
//! `MUSICBRAINZ_ALBUMTYPE` style, ID3v2 uses `TXXX` frames with spaced
//! names ("MusicBrainz Album Type"), and MP4 uses freeform iTunes atoms
//! whose full key carries a `----:com.apple.iTunes:` prefix. Writing the
//! Vorbis spelling everywhere left MP3s with a second, differently named
//! copy beside Picard's and dropped the value outright on MP4 (lofty's
//! writer only accepts the prefixed form), and reading only the Vorbis
//! spelling never matched what Picard-tagged files actually hold — so the
//! rows came back as changes on every open.

use lofty::ape::{ApeItem, ApeTag};
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::{ItemValue, Tag, TagType};

/// A Picard field lofty's generic [`Tag`] has no portable key for, read
/// and written by its per-container spelling (see [`user_fields`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicardField {
    AlbumType,
    AlbumStatus,
    AlbumPackaging,
    ReleaseCountry,
    /// The release's script (`Latn`). Unlike the rest, lofty has a key
    /// for it in Vorbis and MP4; only ID3v2 needs Picard's `TXXX:SCRIPT`.
    Script,
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
    pub const ALL: [PicardField; 7] = [
        PicardField::AlbumType,
        PicardField::AlbumStatus,
        PicardField::AlbumPackaging,
        PicardField::ReleaseCountry,
        PicardField::Script,
        PicardField::AcoustidFingerprint,
        PicardField::AcoustidId,
    ];

    /// Our internal name for the field (it names the tag-manager row), and
    /// its APE spelling.
    pub fn canonical(self) -> &'static str {
        match self {
            PicardField::AlbumType => "MUSICBRAINZ_ALBUMTYPE",
            PicardField::AlbumStatus => "MUSICBRAINZ_ALBUMSTATUS",
            PicardField::AlbumPackaging => "MUSICBRAINZ_ALBUMPACKAGING",
            PicardField::ReleaseCountry => "RELEASECOUNTRY",
            PicardField::Script => "SCRIPT",
            PicardField::AcoustidFingerprint => "ACOUSTID_FINGERPRINT",
            PicardField::AcoustidId => "ACOUSTID_ID",
        }
    }

    /// Picard's Vorbis comment spelling: its internal tag name uppercased.
    /// Album type and status differ from [`Self::canonical`]; packaging is
    /// not a Picard tag at all, so it keeps our own name.
    fn vorbis(self) -> &'static str {
        match self {
            PicardField::AlbumType => "RELEASETYPE",
            PicardField::AlbumStatus => "RELEASESTATUS",
            other => other.canonical(),
        }
    }

    /// Picard's spaced spelling (ID3v2 `TXXX` description, MP4 atom name).
    fn spaced(self) -> &'static str {
        match self {
            PicardField::AlbumType => "MusicBrainz Album Type",
            PicardField::AlbumStatus => "MusicBrainz Album Status",
            PicardField::AlbumPackaging => "MusicBrainz Album Packaging",
            PicardField::ReleaseCountry => "MusicBrainz Album Release Country",
            PicardField::Script => "SCRIPT",
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
            TagType::VorbisComments => self.vorbis().to_string(),
            _ => self.canonical().to_string(),
        }
    }

    /// `key` (as [`user_fields`] reports it, prefix and all) names this
    /// field in any of its spellings. A FLAC or Ogg file tagged before the
    /// Vorbis spelling was fixed holds `MUSICBRAINZ_ALBUMTYPE`; it still
    /// reads, and the next write replaces it with `RELEASETYPE`.
    pub fn matches(self, key: &str) -> bool {
        let name = strip_freeform_prefix(key);
        name.eq_ignore_ascii_case(self.canonical())
            || name.eq_ignore_ascii_case(self.vorbis())
            || name.eq_ignore_ascii_case(self.spaced())
    }

    /// The first non-empty value `tag` holds for this field, under any
    /// spelling.
    pub fn read(self, tag: &Tag) -> Option<String> {
        user_fields(tag)
            .into_iter()
            .find(|(key, value)| !value.is_empty() && self.matches(key))
            .map(|(_, value)| value)
    }

    /// Store `value` under [`Self::key_for`] the tag's container, removing
    /// every other spelling of the field first so a Picard-tagged MP3 does
    /// not end up with two differently named copies. An empty `value` is
    /// ignored; a container with no user-defined fields (RIFF INFO, AIFF
    /// text chunks, ID3v1) is left alone.
    pub fn write(self, tag: &mut Tag, value: &str) {
        if value.is_empty() {
            return;
        }
        let key = self.key_for(tag.tag_type());
        replace_user_fields(tag, |k| self.matches(k), &key, value);
    }
}

/// Remove every user-defined field whose key (as [`user_fields`] spells
/// it) satisfies `drop`, then store `value` under `key`.
///
/// Goes through the concrete tag because that is the only place lofty
/// keeps a key it does not model (see [`user_fields`]); converting back
/// files the fields it does model under their generic keys again.
fn replace_user_fields(tag: &mut Tag, drop: impl Fn(&str) -> bool, key: &str, value: &str) {
    let rebuilt = match tag.tag_type() {
        TagType::Id3v2 => {
            let mut id3 = Id3v2Tag::from(tag.clone());
            id3.retain(|f| !matches!(f, Frame::UserText(t) if drop(&t.description)));
            id3.insert_user_text(key.to_string(), value.to_string());
            Tag::from(id3)
        }
        TagType::VorbisComments => {
            let mut vc = VorbisComments::from(tag.clone());
            let held: Vec<String> = vc
                .items()
                .filter(|(k, _)| drop(k))
                .map(|(k, _)| k.to_string())
                .collect();
            for k in held {
                vc.remove(&k).for_each(std::mem::drop);
            }
            vc.insert(key.to_string(), value.to_string());
            Tag::from(vc)
        }
        TagType::Mp4Ilst => {
            let Some((mean, name)) = key.strip_prefix("----:").and_then(|r| r.split_once(':'))
            else {
                return;
            };
            let mut ilst = Ilst::from(tag.clone());
            ilst.retain(|a| !matches!(freeform_key(a.ident()), Some(k) if drop(&k)));
            ilst.insert(Atom::new(
                AtomIdent::Freeform {
                    mean: mean.to_string().into(),
                    name: name.to_string().into(),
                },
                AtomData::UTF8(value.to_string()),
            ));
            Tag::from(ilst)
        }
        TagType::Ape => {
            let Ok(item) = ApeItem::new(key.to_string(), ItemValue::Text(value.to_string())) else {
                return;
            };
            let mut ape = ApeTag::from(tag.clone());
            let held: Vec<String> = (&ape)
                .into_iter()
                .filter(|i| drop(i.key()))
                .map(|i| i.key().to_string())
                .collect();
            for k in held {
                ape.remove(&k);
            }
            ape.insert(item);
            Tag::from(ape)
        }
        _ => return,
    };
    *tag = rebuilt;
}

/// Store `value` under exactly `key` (spelled as [`user_fields`] reports
/// it), for tests that seed a tag the way another tagger left it.
#[cfg(test)]
pub(crate) fn insert_user_field(tag: &mut Tag, key: &str, value: &str) {
    replace_user_fields(tag, |k| k == key, key, value);
}

fn freeform_key(ident: &AtomIdent<'_>) -> Option<String> {
    match ident {
        AtomIdent::Freeform { mean, name } => Some(format!("----:{mean}:{name}")),
        AtomIdent::Fourcc(_) => None,
    }
}

/// Every user-defined text field `tag` holds, as `(key, value)`: ID3v2
/// `TXXX` frames by description, Vorbis comments and APE items by name,
/// MP4 freeform atoms as `----:<mean>:<name>`.
///
/// lofty's generic [`Tag`] has no key for a field it does not model; it
/// keeps those in a format-specific companion tag that only a conversion
/// back to the concrete tag exposes. The conversion also re-emits the
/// fields lofty does model (`ACOUSTID_ID`, `SCRIPT`…) under the spelling
/// it writes, so a field reads the same whether lofty knows it or not.
pub fn user_fields(tag: &Tag) -> Vec<(String, String)> {
    match tag.tag_type() {
        TagType::Id3v2 => {
            let id3 = Id3v2Tag::from(tag.clone());
            (&id3)
                .into_iter()
                .filter_map(|f| match f {
                    Frame::UserText(t) => Some((t.description.to_string(), t.content.to_string())),
                    _ => None,
                })
                .collect()
        }
        TagType::VorbisComments => VorbisComments::from(tag.clone())
            .items()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        TagType::Mp4Ilst => {
            let ilst = Ilst::from(tag.clone());
            (&ilst)
                .into_iter()
                .filter_map(|a| {
                    let key = freeform_key(a.ident())?;
                    let value = a.data().find_map(|d| match d {
                        AtomData::UTF8(s) => Some(s.clone()),
                        _ => None,
                    })?;
                    Some((key, value))
                })
                .collect()
        }
        TagType::Ape => {
            let ape = ApeTag::from(tag.clone());
            (&ape)
                .into_iter()
                .filter_map(|i| match i.value() {
                    ItemValue::Text(v) => Some((i.key().to_string(), v.clone())),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_picard_per_container() {
        assert_eq!(
            PicardField::AlbumType.key_for(TagType::VorbisComments),
            "RELEASETYPE"
        );
        assert_eq!(
            PicardField::AlbumStatus.key_for(TagType::VorbisComments),
            "RELEASESTATUS"
        );
        assert_eq!(
            PicardField::AlbumType.key_for(TagType::Ape),
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
    fn script_uses_lofty_key_except_in_id3v2() {
        use lofty::tag::ItemKey;
        let written = |tag_type| {
            let mut tag = Tag::new(tag_type);
            PicardField::Script.write(&mut tag, "Latn");
            tag
        };
        let id3 = written(TagType::Id3v2);
        assert_eq!(id3.get_string(ItemKey::Script), None);
        assert_eq!(
            user_fields(&id3),
            vec![("SCRIPT".to_string(), "Latn".to_string())]
        );
        for tag_type in [TagType::Mp4Ilst, TagType::VorbisComments] {
            let tag = written(tag_type);
            assert_eq!(
                tag.get_string(ItemKey::Script),
                Some("Latn"),
                "{tag_type:?}"
            );
            assert!(!tag.has_format_specific_items(), "{tag_type:?}");
        }
        assert!(PicardField::Script.matches("----:com.apple.iTunes:SCRIPT"));
        let mut id3 = Tag::new(TagType::Id3v2);
        PicardField::AlbumType.write(&mut id3, "album");
        assert_eq!(
            user_fields(&id3),
            vec![("MusicBrainz Album Type".to_string(), "album".to_string())]
        );
    }

    #[test]
    fn write_replaces_every_spelling_and_reads_back_in_each_container() {
        for tag_type in [
            TagType::Id3v2,
            TagType::VorbisComments,
            TagType::Mp4Ilst,
            TagType::Ape,
        ] {
            for field in PicardField::ALL {
                let mut tag = Tag::new(tag_type);
                field.write(&mut tag, "old");
                field.write(&mut tag, "new");
                assert_eq!(
                    field.read(&tag).as_deref(),
                    Some("new"),
                    "{tag_type:?} {field:?}"
                );
                let held: Vec<_> = user_fields(&tag)
                    .into_iter()
                    .filter(|(k, _)| field.matches(k))
                    .collect();
                // APE reports SCRIPT under lofty's own spelling, `Script`.
                assert_eq!(held.len(), 1, "{tag_type:?} {field:?}: {held:?}");
                assert!(
                    held[0].0.eq_ignore_ascii_case(&field.key_for(tag_type)) && held[0].1 == "new",
                    "{tag_type:?} {field:?}: {held:?}"
                );
            }
        }
    }

    #[test]
    fn a_vorbis_album_type_in_the_old_spelling_is_replaced_by_picards() {
        for (field, old, new) in [
            (
                PicardField::AlbumType,
                "MUSICBRAINZ_ALBUMTYPE",
                "RELEASETYPE",
            ),
            (
                PicardField::AlbumStatus,
                "MUSICBRAINZ_ALBUMSTATUS",
                "RELEASESTATUS",
            ),
        ] {
            let mut tag = Tag::new(TagType::VorbisComments);
            insert_user_field(&mut tag, old, "album");
            assert_eq!(field.read(&tag).as_deref(), Some("album"), "{old}");
            field.write(&mut tag, "single");
            let held: Vec<_> = user_fields(&tag)
                .into_iter()
                .filter(|(k, _)| field.matches(k))
                .collect();
            assert_eq!(held.len(), 1, "{held:?}");
            assert!(held[0].0.eq_ignore_ascii_case(new), "{held:?}");
            assert_eq!(held[0].1, "single");
        }
    }

    #[test]
    fn matches_every_spelling_lofty_reports() {
        let f = PicardField::AlbumStatus;
        assert!(f.matches("MUSICBRAINZ_ALBUMSTATUS"));
        assert!(f.matches("musicbrainz_albumstatus"));
        assert!(f.matches("RELEASESTATUS"));
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
