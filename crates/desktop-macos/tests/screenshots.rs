//! macOS-only integration check: a real capture of every active display, a
//! rect crop, and the error path for a window id that matches nothing. Skips
//! with a message when Screen Recording permission is not granted to this
//! process.

#![cfg(target_os = "macos")]

use desktop_api::{Quality, Region};
use desktop_macos::capture;

#[test]
fn captures_all_active_displays_when_screen_recording_is_granted() {
    if !capture::screen_recording_granted() {
        eprintln!(
            "skipped: Screen Recording permission is not granted for this process \
             (enable it in System Settings > Privacy & Security > Screen Recording)"
        );
        return;
    }
    let displays = desktop_macos::displays::active_displays().expect("display list");
    assert!(!displays.is_empty(), "an active display exists");
    let points_w: f64 = displays
        .iter()
        .map(|d| d.bounds.x + d.bounds.width)
        .fold(f64::MIN, f64::max)
        - displays.iter().map(|d| d.bounds.x).fold(f64::MAX, f64::min);
    let image = capture::capture(&Region::FullScreen, Quality::Lossless).expect("full capture");
    assert_eq!(image.mime_type, "image/png");
    assert!(
        image
            .data
            .starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
    );
    // The stitched canvas covers every display: at least one pixel per display
    // point (± a pixel of rounding).
    assert!(
        f64::from(image.width) >= points_w - 1.0,
        "canvas width {} covers the displays ({points_w} points)",
        image.width
    );
    assert!(image.height > 0);
}

#[test]
fn a_rect_capture_returns_the_rect_at_the_captured_scale() {
    if !capture::screen_recording_granted() {
        eprintln!("skipped: Screen Recording permission is not granted for this process");
        return;
    }
    let displays = desktop_macos::displays::active_displays().expect("display list");
    let (min_x, min_y) = (
        displays.iter().map(|d| d.bounds.x).fold(f64::MAX, f64::min),
        displays.iter().map(|d| d.bounds.y).fold(f64::MAX, f64::min),
    );
    let points_w = displays
        .iter()
        .map(|d| d.bounds.x + d.bounds.width)
        .fold(f64::MIN, f64::max)
        - min_x;
    // The capture scale comes from the returned image, not from
    // `CGDisplayPixelsWide` (which under scaled modes reports the logical
    // size): derive it from a full capture, exactly as the capture itself does.
    let full = capture::capture(&Region::FullScreen, Quality::Lossless).expect("full capture");
    let scale = f64::from(full.width) / points_w;
    let rect = desktop_api::snapshot::Rect {
        x: min_x + 10.0,
        y: min_y + 10.0,
        width: 100.0,
        height: 100.0,
    };
    let image = capture::capture(&Region::Rect(rect), Quality::Lossless).expect("rect capture");
    // The crop is `100x100` points at the captured scale (± a pixel of rounding).
    assert!(
        (f64::from(image.width) - 100.0 * scale).abs() <= 1.0,
        "crop width {} is 100 points at {scale}x",
        image.width
    );
    assert!(
        (f64::from(image.height) - 100.0 * scale).abs() <= 1.0,
        "crop height {} is 100 points at {scale}x",
        image.height
    );
}

#[test]
fn a_lossy_capture_is_jpeg() {
    if !capture::screen_recording_granted() {
        eprintln!("skipped: Screen Recording permission is not granted for this process");
        return;
    }
    let image = capture::capture(&Region::FullScreen, Quality::Low).expect("full capture");
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
