//! Inbox location and canonical `{AlbumArtist}/{Album}` path helpers.
//!
//! The drop folder is a **sibling** of `music_dir` (`../Automatically Add to
//! Music`) so it never sorts among artist folders. Filing itself goes through
//! the tag-manager overlay (MusicBrainz diff + rename).

use std::path::{Path, PathBuf};

/// Folder name next to the music library root (not inside it).
pub const INBOX_DIR_NAME: &str = "Automatically Add to Music";

/// `{parent(music_dir)}/Automatically Add to Music`. `None` when `music_dir`
/// has no parent (`/` or a relative `.`).
pub fn default_inbox_dir(music_dir: &Path) -> Option<PathBuf> {
    music_dir.parent().map(|p| p.join(INBOX_DIR_NAME))
}

/// Replace filesystem-hostile characters in a path component.
pub fn sanitise_filename_component(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "Unknown".to_string();
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        "Unknown".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_is_sibling_of_music_dir() {
        let music = PathBuf::from("/home/you/Music");
        assert_eq!(
            default_inbox_dir(&music).as_deref(),
            Some(Path::new("/home/you/Automatically Add to Music"))
        );
        assert!(default_inbox_dir(Path::new("/")).is_none());
    }

    #[test]
    fn sanitise_filename_replaces_path_separators() {
        assert_eq!(sanitise_filename_component("AC/DC"), "AC_DC");
        assert_eq!(sanitise_filename_component("a:b"), "a_b");
        assert_eq!(sanitise_filename_component(""), "Unknown");
        assert_eq!(sanitise_filename_component("Foo..."), "Foo");
        assert_eq!(sanitise_filename_component("Foo?Bar*"), "Foo_Bar_");
        assert_eq!(sanitise_filename_component("a\x01b\x02c"), "abc");
        assert_eq!(sanitise_filename_component("   "), "Unknown");
        assert_eq!(sanitise_filename_component("///"), "___");
        assert_eq!(sanitise_filename_component("Foo   "), "Foo");
        assert_eq!(sanitise_filename_component("\x01\x02\x03"), "Unknown");
    }
}
