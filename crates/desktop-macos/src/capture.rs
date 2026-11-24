//! CoreGraphics screen and window capture for the macOS backend.
//!
//! The capture functions are `deprecated`/obsoleted in the macOS 15 SDK
//! headers, which point to `ScreenCaptureKit` as the replacement, but they are
//! present and working at runtime (verified on macOS 26.7: preflight true,
//! real 2940×1912 BGRA pixels). Migrating to `ScreenCaptureKit` is a follow-up.

#![allow(deprecated)] // legacy CG capture APIs, see the module comment

use std::time::Duration;

use desktop_api::{
    DesktopError,
    snapshot::{Image, Quality, Region},
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDataProvider, CGDisplayBounds, CGDisplayCreateImage, CGDisplayPixelsWide, CGImage,
    CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGWindowID,
    CGWindowImageOption, CGWindowListCreateImage, CGWindowListOption,
};

use crate::pure;

/// A wedged window server should fail fast instead of stalling the agent loop.
pub const CAPTURE_LIMIT: Duration = Duration::from_secs(8);

/// `CGRectNull`: tells `CGWindowListCreateImage` to use the bounding box of
/// the listed windows instead of cropping to a screen rect. The constant is
/// not exposed by the bindings; this is its documented runtime value
/// (`{origin: (+inf, +inf), size: 0×0}`, read from CoreGraphics on macOS 26.7).
#[must_use]
fn cg_rect_null() -> CGRect {
    CGRect {
        origin: CGPoint {
            x: f64::INFINITY,
            y: f64::INFINITY,
        },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    }
}

/// Whether Screen Recording permission is currently granted.
#[must_use]
pub fn screen_recording_granted() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Ask macOS for Screen Recording permission (shows the system prompt once; a
/// previously denied program is never re-prompted, the user must enable it in
/// System Settings).
#[must_use]
pub fn request_screen_recording() -> bool {
    CGRequestScreenCaptureAccess()
}

/// Capture `region` from the main display or one window.
///
/// `Region::FullScreen` and `Region::Rect` cover the main display; capturing
/// every monitor stitched into one image is a follow-up (the issue scopes this
/// slice to "the main display and of a window").
///
/// # Errors
/// [`DesktopError::Screenshot`] when Screen Recording is not granted, the
/// window matches nothing, or CoreGraphics returns no image.
pub fn capture(region: &Region, quality: Quality) -> Result<Image, DesktopError> {
    if !screen_recording_granted() {
        return Err(DesktopError::Screenshot(
            "Screen Recording permission is not granted: enable it for this program in \
             System Settings > Privacy & Security > Screen Recording, then retry"
                .into(),
        ));
    }
    match region {
        Region::FullScreen | Region::Rect(_) => capture_display(region, quality),
        Region::Window { window_id } => capture_window(*window_id, quality),
    }
}

fn capture_display(region: &Region, quality: Quality) -> Result<Image, DesktopError> {
    let display = CGMainDisplayID();
    let cg = CGDisplayCreateImage(display)
        .ok_or_else(|| DesktopError::Screenshot("the display capture returned no image".into()))?;
    let width = image_width(&cg)?;
    let height = image_height(&cg)?;

    let data = image_data(&cg)?;
    let mut rgba = pure::trim_bgra_rows(&data, CGImage::bytes_per_row(Some(&cg)), width, height);
    pure::bgra_to_rgba(&mut rgba);

    if let Region::Rect(rect) = region {
        let scale = pure::pixel_scale(
            CGDisplayPixelsWide(display),
            CGDisplayBounds(display).size.width,
        );
        let (pixels, width, height) = pure::crop_rect(&rgba, width, height, rect, scale)?;
        return pure::encode_rgba(&pixels, width, height, quality);
    }
    pure::encode_rgba(&rgba, width, height, quality)
}

fn capture_window(window_id: u64, quality: Quality) -> Result<Image, DesktopError> {
    let id = CGWindowID::try_from(window_id)
        .map_err(|_| DesktopError::Screenshot(format!("window id {window_id} is out of range")))?;
    let cg = CGWindowListCreateImage(
        cg_rect_null(),
        CGWindowListOption::OptionIncludingWindow,
        id,
        CGWindowImageOption::Default,
    )
    .ok_or_else(|| DesktopError::Screenshot("the window capture returned no image".into()))?;

    let width = image_width(&cg)?;
    let height = image_height(&cg)?;
    let data = image_data(&cg)?;
    let mut rgba = pure::trim_bgra_rows(&data, CGImage::bytes_per_row(Some(&cg)), width, height);
    pure::bgra_to_rgba(&mut rgba);

    if pure::is_fully_transparent(&rgba) {
        // CoreGraphics returns a transparent black image for a window id that
        // matches nothing (or shares no pixels); report that as an error.
        return Err(DesktopError::Screenshot(format!(
            "window {window_id} matches no capturable window (it may be closed or hidden)"
        )));
    }
    pure::encode_rgba(&rgba, width, height, quality)
}

fn image_width(cg: &CGImage) -> Result<u32, DesktopError> {
    u32::try_from(CGImage::width(Some(cg)))
        .map_err(|_| DesktopError::Screenshot("captured image width is out of range".into()))
}

fn image_height(cg: &CGImage) -> Result<u32, DesktopError> {
    u32::try_from(CGImage::height(Some(cg)))
        .map_err(|_| DesktopError::Screenshot("captured image height is out of range".into()))
}

fn image_data(cg: &CGImage) -> Result<Vec<u8>, DesktopError> {
    let provider = CGImage::data_provider(Some(cg)).ok_or_else(|| {
        DesktopError::Screenshot("the captured image has no data provider".into())
    })?;
    let data = CGDataProvider::data(Some(&provider)).ok_or_else(|| {
        DesktopError::Screenshot("the captured image data could not be read".into())
    })?;
    Ok(data.to_vec())
}
