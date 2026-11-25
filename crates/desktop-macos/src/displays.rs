//! Active display enumeration via CoreGraphics.
//!
//! `FullScreen` and `Rect` captures stitch every active display, so capture
//! needs the position, size and pixel scale of each display.

#![allow(unsafe_code)] // CGGetActiveDisplayList has no safe wrapper in objc2-core-graphics;
// the single unsafe block below passes a valid out-pointer and a buffer sized
// by the first call, and every other access goes through the safe bindings.

use desktop_api::{DesktopError, snapshot::Rect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayPixelsHigh, CGDisplayPixelsWide, CGError,
    CGGetActiveDisplayList,
};

/// One active display: its id, bounds in global display points, and the pixel
/// size `CGDisplayPixelsWide/High` report.
///
/// Note: under a scaled ("looks like") display mode these pixel sizes can be
/// the logical size, not the backing store — on a `Retina` `MacBook`,
/// `CGDisplayPixelsWide` reports 1470 while `CGDisplayCreateImage` returns
/// 2940×1912 device pixels. The capture scale is therefore always derived
/// from the size of the returned image, never from these fields.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: CGDirectDisplayID,
    pub bounds: Rect,
    pub pixels_w: u32,
    pub pixels_h: u32,
}

/// List every active display.
///
/// # Errors
/// [`DesktopError::Screenshot`] when CoreGraphics reports an error or returns
/// an implausible count.
pub fn active_displays() -> Result<Vec<DisplayInfo>, DesktopError> {
    let err = || DesktopError::Screenshot("the display list could not be read".into());
    // Ask for a fixed-size batch first; re-allocate and retry once if a
    // display was connected between the two calls (the count grew). This keeps
    // the FFI surface to out-pointers we own.
    let mut buffer = [0_u32; 16];
    let batch = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let ids = {
        let mut count = batch;
        let success = CGError::Success;
        // SAFETY: `buffer` is a valid out-pointer for `count` entries and
        // `count` a valid out-pointer; the call only writes what it reports.
        let error = unsafe { CGGetActiveDisplayList(batch, buffer.as_mut_ptr(), &raw mut count) };
        if error != success {
            return Err(err());
        }
        if count > batch {
            // More displays than the batch held (hotplug between calls):
            // retry once with an exact buffer.
            let mut grown = vec![0_u32; count as usize];
            let mut count2 = count;
            // SAFETY: `grown` holds exactly `count2` entries.
            let error =
                unsafe { CGGetActiveDisplayList(count, grown.as_mut_ptr(), &raw mut count2) };
            if error != success || count2 > count {
                return Err(err());
            }
            count = count2;
            grown.truncate(count as usize);
            grown
        } else {
            buffer[..count as usize].to_vec()
        }
    };

    let mut infos = Vec::with_capacity(ids.len());
    for id in ids {
        let cg_bounds = CGDisplayBounds(id);
        let (Some(points_w), Some(points_h)) = (
            finite_positive(cg_bounds.size.width),
            finite_positive(cg_bounds.size.height),
        ) else {
            return Err(err());
        };
        let (Ok(pixels_w), Ok(pixels_h)) = (
            u32::try_from(CGDisplayPixelsWide(id)),
            u32::try_from(CGDisplayPixelsHigh(id)),
        ) else {
            return Err(err());
        };
        infos.push(DisplayInfo {
            id,
            bounds: Rect {
                x: cg_bounds.origin.x,
                y: cg_bounds.origin.y,
                width: points_w,
                height: points_h,
            },
            pixels_w,
            pixels_h,
        });
    }
    Ok(infos)
}

/// A finite, positive dimension of a display.
fn finite_positive(value: f64) -> Option<f64> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}
