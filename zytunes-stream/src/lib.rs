//! HTTP streaming API for a zytunes directory library.
//!
//! Browse, search, stream with Range, on-demand stem splits served
//! from the TUI stem cache, and client-reported plays into the TUI
//! sidecar. See `docs/stream-api.md`.

mod art;
mod auth;
mod dto;
mod range;
mod search;
mod serve;
mod stems;

pub use art::load_track_art;
pub use auth::require_bearer;
pub use dto::{AlbumPair, PlayRecord, SearchResults, TrackDetail, TrackSummary};
pub use range::{content_type_for, parse_byte_range, ByteRange};
pub use search::search_library;
pub use serve::{build_router, AppState};
pub use stems::{EngineLookup, StemHub, StemSetDto, StemSettings};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zytunes::library::{MusicLibrary, Track};

/// Resolve a track's on-disk path only when it sits inside `music_root`.
pub fn resolved_track_path(track: &Track, music_root: &Path) -> Option<PathBuf> {
    let loc = track.location.as_deref()?;
    let path = PathBuf::from(loc);
    if !path.is_file() {
        return None;
    }
    let root = music_root.canonicalize().ok()?;
    let canon = path.canonicalize().ok()?;
    if canon.starts_with(&root) {
        // Containment is checked on the canonicalized path, but the
        // ORIGINAL library path is returned: stem-cache entries are keyed
        // by a hash of the path string the TUI saw at split time, so
        // returning `canon` breaks cache hits whenever the library root
        // involves a symlink (e.g. /var → /private/var on macOS).
        Some(path)
    } else {
        None
    }
}

/// Shared library handle used by handlers.
pub type LibraryHandle = Arc<dyn MusicLibrary + Send + Sync>;
