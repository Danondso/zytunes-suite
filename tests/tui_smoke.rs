//! In-process smoke tests for the TUI. Drives the real `App` against a
//! `TestBackend` through the harness so each scenario flows through the same
//! key/event dispatchers production uses.
#![cfg(feature = "tui-testing")]

use crossterm::event::KeyCode;
use zytunes::background::{BgCommand, BgEvent, DeviceInfo, StorageInfo};
use zytunes::device::DeviceFamily;
use zytunes::testing::{Harness, MockLibrary};

/// Boots the harness, ticks once, and snapshots the rendered buffer. The
/// loading-library banner should appear because `App::new()` defaults
/// `loading_library` to false but the binary flips it on at startup — we
/// flip it on here too so the snapshot reflects what users actually see at
/// process start.
#[test]
fn boots_to_loading_state() {
    let mut h = Harness::new(120, 40);
    h.app.loading_library = true;
    h.step();

    let frame = h.render();
    insta::assert_snapshot!("boots_to_loading_state", frame);
}

/// `LibraryLoaded` populates the sidebar artist list and clears the loading
/// flag. Sidebar should hold the two distinct artists from `MockLibrary`.
#[test]
fn library_loaded_populates_sidebar() {
    let mut h = Harness::new(120, 40);
    h.app.loading_library = true;
    h.push_event(BgEvent::LibraryLoaded(Ok(Box::new(MockLibrary::small()))));
    h.step();

    assert!(!h.app.loading_library);
    assert_eq!(h.app.sidebar_items.len(), 2);
    assert!(!h.app.sidebar_items.is_empty());

    let frame = h.render();
    insta::assert_snapshot!("library_loaded_populates_sidebar", frame);
}

/// Connect flow: `DeviceDetected` flips status to Connecting, `SessionReady`
/// flips it to Connected, `DeviceTracksLoaded` populates the device index,
/// then pressing `v` toggles into Device browse mode. The harness should not
/// emit any user-visible commands during the flow (the binary's `c` handler
/// sends `Connect`, but we skipped that step so cmd_rx stays empty).
#[test]
fn device_connect_flow() {
    let mut h = Harness::new(120, 40);

    // Library has to be loaded first so the App can correlate device tracks.
    h.push_event(BgEvent::LibraryLoaded(Ok(Box::new(MockLibrary::small()))));
    h.step();

    h.push_event(BgEvent::DeviceDetected(DeviceInfo {
        name: "Zune 30 (test)".into(),
        firmware_version: Some("3.30".into()),
        serial_number: Some("TEST-0001".into()),
        usb_mode: Some("MTPZ".into()),
        manufacturer: Some("Microsoft".into()),
        model: Some("Zune 30".into()),
        family: DeviceFamily::Zune,
    }));
    h.push_event(BgEvent::SessionReady(Some(StorageInfo {
        used_bytes: 1_000_000_000,
        free_bytes: 28_000_000_000,
        total_bytes: 29_000_000_000,
        used_percent: 4,
    })));
    h.push_event(BgEvent::DeviceTracksLoaded(vec![device_entry(
        100,
        "Radiohead/Kid A/Idioteque.mp3",
    )]));
    h.step();

    assert_eq!(
        h.app.device.status,
        zytunes::app::DeviceStatus::Connected,
        "session-ready should flip status to Connected"
    );
    assert_eq!(h.app.device.tracks.len(), 1);

    // Switch to Device browse mode through the public dispatcher.
    let outcome = h.key(KeyCode::Char('v'));
    assert_eq!(outcome, zytunes::app::KeyOutcome::Continue);
    assert_eq!(h.app.browse_mode, zytunes::app::BrowseMode::Device);

    // `v` doesn't enqueue any bg commands; the only commands the App might
    // have enqueued are auto-photo/video sync ones if env vars are set, so
    // tolerate those rather than asserting an exact empty list.
    let cmds = h.drain_commands();
    assert!(
        cmds.iter().all(|c| matches!(
            c,
            BgCommand::SyncPhotos { .. } | BgCommand::SyncVideos { .. }
        )),
        "unexpected bg commands: {:?}",
        cmds.iter().map(label).collect::<Vec<_>>()
    );

    let frame = h.render();
    insta::assert_snapshot!("device_connect_flow", frame);
}

fn device_entry(object_id: u64, name: &str) -> zytunes::mtp::parse::DeviceEntry {
    zytunes::mtp::parse::DeviceEntry {
        object_id,
        storage_id: 0,
        format: "MP3".to_string(),
        size: 4_000_000,
        name: name.to_string(),
        track_number: None,
        disc_number: None,
        play_count: None,
        last_played: None,
        skip_count: None,
        rating: None,
    }
}

fn label(cmd: &BgCommand) -> &'static str {
    match cmd {
        BgCommand::LoadLibrary { .. } => "LoadLibrary",
        BgCommand::Connect => "Connect",
        BgCommand::LoadDeviceTracks => "LoadDeviceTracks",
        BgCommand::Disconnect => "Disconnect",
        BgCommand::ExecuteSyncQueue(_) => "ExecuteSyncQueue",
        BgCommand::AppendSyncQueue(_) => "AppendSyncQueue",
        BgCommand::RemoveFromDevice(_) => "RemoveFromDevice",
        BgCommand::SyncPhotos { .. } => "SyncPhotos",
        BgCommand::SyncVideos { .. } => "SyncVideos",
        BgCommand::CancelSync => "CancelSync",
        BgCommand::LoadAlbumArt { .. } => "LoadAlbumArt",
    }
}
