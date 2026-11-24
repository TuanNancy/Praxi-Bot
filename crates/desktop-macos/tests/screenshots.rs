//! macOS-only integration check: a real capture of the main display, and the
//! error path for a window id that matches nothing. Skips with a message when
//! Screen Recording permission is not granted to this process.

#![cfg(target_os = "macos")]

use desktop_api::{Quality, Region};
use desktop_macos::capture;

#[test]
fn captures_the_main_display_when_screen_recording_is_granted() {
    if !capture::screen_recording_granted() {
        eprintln!(
            "skipped: Screen Recording permission is not granted for this process \
             (enable it in System Settings > Privacy & Security > Screen Recording)"
        );
        return;
    }
    let image =
        capture::capture(&Region::FullScreen, Quality::Lossless).expect("main display capture");
    assert_eq!(image.mime_type, "image/png");
    assert!(
        image
            .data
            .starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
    );
    assert!(image.width > 0);
    assert!(image.height > 0);
}

#[test]
fn a_lossy_capture_is_jpeg() {
    if !capture::screen_recording_granted() {
        eprintln!("skipped: Screen Recording permission is not granted for this process");
        return;
    }
    let image = capture::capture(&Region::FullScreen, Quality::Low).expect("main display capture");
    assert_eq!(image.mime_type, "image/jpeg");
    assert!(image.data.starts_with(&[0xFF, 0xD8]));
}

#[test]
fn a_window_id_that_matches_nothing_is_an_error() {
    if !capture::screen_recording_granted() {
        eprintln!("skipped: Screen Recording permission is not granted for this process");
        return;
    }
    let err = capture::capture(
        &Region::Window {
            window_id: u64::from(u32::MAX),
        },
        Quality::Low,
    );
    assert!(
        err.is_err(),
        "a nonexistent window must error, not return an image"
    );
}
