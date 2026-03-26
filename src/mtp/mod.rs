pub mod aft;
pub mod native;
pub mod parse;

pub use aft::AftSession;
pub use native::NativeSession;
use parse::DeviceEntry;

/// Trait abstracting device session operations for testability.
pub trait DeviceSession {
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    fn zune_import(&mut self, local_path: &str) -> Result<u64, String>;
    fn rm(&mut self, device_path: &str) -> Result<(), String>;
    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    fn create_playlist(&mut self, name: &str, track_ids: &[u64]) -> Result<(), String>;
}
