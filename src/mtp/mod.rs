pub mod native;
pub mod parse;
pub mod zmdb;

pub use native::NativeSession;
use parse::DeviceEntry;

/// Trait abstracting device session operations for testability.
///
/// Implemented by [`NativeSession`] for real hardware and by `MockSession` in tests.
/// All methods return `Result<T, String>` — errors are human-readable messages.
pub trait DeviceSession {
    /// List files and directories at `path` on the device.
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    /// Import a local audio file to the device. Returns the new MTP object ID.
    fn zune_import(&mut self, local_path: &str) -> Result<u64, String>;
    /// Remove a file or folder by device path (e.g., `/Music/Artist/Album/track.mp3`).
    fn rm(&mut self, device_path: &str) -> Result<(), String>;
    /// Remove an object by its MTP object ID.
    fn rm_by_id(&mut self, object_id: u32) -> Result<(), String>;
    /// Delete empty artist/album folders under `/Music`. Returns count of folders removed.
    fn cleanup_empty_folders(&mut self) -> Result<usize, String>;
    /// Query device storage. Returns `(total_bytes, free_bytes)`.
    fn get_storage_info(&mut self) -> Result<(u64, u64), String>;
    /// Recursively collect all track entries under `path` (e.g., `/Music`).
    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    /// Create a playlist on the device with the given name and track object IDs.
    fn create_playlist(&mut self, name: &str, track_ids: &[u64]) -> Result<(), String>;
    /// Save the device's sync progress to a local cache file.
    /// Called after a successful sync session. Default is a no-op.
    fn save_sync_progress(&mut self) {}
    /// Import a photo to the device. Takes filename and pre-resized JPEG bytes.
    /// Returns the new MTP object ID.
    fn import_photo(&mut self, _filename: &str, _jpeg_data: &[u8]) -> Result<u64, String> {
        Err("Photo import not supported".into())
    }
    /// Import a video file to the device. Takes filename and raw file bytes.
    /// Returns the new MTP object ID.
    fn import_video(&mut self, _filename: &str, _data: &[u8]) -> Result<u64, String> {
        Err("Video import not supported".into())
    }
}
