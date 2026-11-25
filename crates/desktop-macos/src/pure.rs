//! Platform-independent parts of the macOS backend: BGRA→RGBA pixel
//! conversion, row-stride trimming, rect cropping and JPEG/PNG encoding.
//!
//! Compiled on every OS so that all pure logic is unit-testable on Windows,
//! Linux, and macOS (CI has no macOS runner).

use desktop_api::{
    DesktopError,
    snapshot::{Image, Quality, Rect},
};

/// JPEG quality level for a [`Quality`] setting, matching the Windows backend.
#[must_use]
pub fn jpeg_quality(quality: Quality) -> u8 {
    match quality {
        Quality::Low => 40,
        Quality::Medium => 70,
        Quality::High | Quality::Lossless => 90,
    }
}

/// Trim a raw BGRA capture to `height` rows of `width * 4` bytes: CoreGraphics
/// pads rows to an alignment (so the stride can exceed `width * 4`) and may
/// hand back one row more than the image height (observed on macOS 26.7).
#[must_use]
pub fn trim_bgra_rows(data: &[u8], bytes_per_row: usize, width: u32, height: u32) -> Vec<u8> {
    let row_len = width as usize * 4;
    let rows = height as usize;
    if bytes_per_row < row_len || data.len() < bytes_per_row {
        return data.to_vec();
    }
    let mut buffer = Vec::with_capacity(row_len * rows);
    for row in data.chunks_exact(bytes_per_row).take(rows) {
        buffer.extend_from_slice(&row[..row_len]);
    }
    buffer
}

/// Convert BGRA to RGBA in place and force opaque alpha (GDI-style captures
/// leave the alpha channel zero; see the Windows backend for the same step).
pub fn bgra_to_rgba(buffer: &mut [u8]) {
    for px in buffer.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
}

/// Scale factor from display points to captured pixels for one display.
///
/// `CGDisplayCreateImage` returns device pixels while `Region` coordinates are
/// in display points, so the factor is the pixel width divided by the point
/// width of the same display.
#[must_use]
pub fn pixel_scale(pixels_wide: usize, points_wide: f64) -> f64 {
    if points_wide <= 0.0 {
        return 1.0;
    }
    let pixels = f64::from(u32::try_from(pixels_wide).unwrap_or(u32::MAX));
    (pixels / points_wide).max(1.0)
}

/// Crop `rect` (in display points) out of a full-display RGBA capture.
///
/// Returns the cropped pixels and their width/height. The rect is clamped to
/// the image; a rect that lies outside it or collapses to nothing is an error.
///
/// # Errors
/// [`DesktopError::Screenshot`] when the rect covers no pixels.
pub fn crop_rect(
    rgba: &[u8],
    width: u32,
    height: u32,
    rect: &Rect,
    scale: f64,
) -> Result<(Vec<u8>, u32, u32), DesktopError> {
    let err = || DesktopError::Screenshot("empty capture region".into());
    #[allow(clippy::cast_possible_truncation)] // clamped to u32 bounds right after
    let clamp = |v: f64| v.max(0.0).min(f64::from(u32::MAX));
    // `clamp` bounds every value to [0, u32::MAX], so the cast can neither
    // truncate nor turn negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let as_u32 = |v: f64| v as u32;

    let left = as_u32(clamp((rect.x * scale).floor()));
    let top = as_u32(clamp((rect.y * scale).floor()));
    let right = as_u32(clamp(((rect.x + rect.width) * scale).ceil())).min(width);
    let bottom = as_u32(clamp(((rect.y + rect.height) * scale).ceil())).min(height);

    if left >= right || top >= bottom {
        return Err(err());
    }
    let (w, h) = (right - left, bottom - top);
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for y in top..bottom {
        let start = y as usize * stride + left as usize * 4;
        out.extend_from_slice(&rgba[start..start + w as usize * 4]);
    }
    Ok((out, w, h))
}

/// Whether every pixel is fully transparent: what CoreGraphics returns for a
/// window id that matches nothing (or a window that shares no pixels).
#[must_use]
pub fn is_fully_transparent(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 0)
}

/// One captured display, ready to be stitched into a combined canvas.
///
/// `bounds` is the display's position in global display points (the main
/// display's origin is the global origin, so a display to its left has a
/// negative `x`), `pixels_w`/`pixels_h` are the captured pixel dimensions and
/// `rgba` holds exactly `pixels_w * pixels_h` RGBA-8 pixels.
pub struct DisplayShot {
    pub bounds: Rect,
    pub pixels_w: u32,
    pub pixels_h: u32,
    pub rgba: Vec<u8>,
}

/// Union of every display's bounds: the canvas all displays are placed onto.
/// Returns `None` when there are no shots.
#[must_use]
pub fn virtual_bounds(shots: &[DisplayShot]) -> Option<Rect> {
    let first = shots.first()?;
    let (mut left, mut top, mut right, mut bottom) = (
        first.bounds.x,
        first.bounds.y,
        first.bounds.x + first.bounds.width,
        first.bounds.y + first.bounds.height,
    );
    for shot in &shots[1..] {
        left = left.min(shot.bounds.x);
        top = top.min(shot.bounds.y);
        right = right.max(shot.bounds.x + shot.bounds.width);
        bottom = bottom.max(shot.bounds.y + shot.bounds.height);
    }
    Some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// Canvas scale: the highest display scale (pixels per point) across all
/// displays, at least 1.0.
///
/// A single 2x display keeps today's resolution (the canvas is as wide as the
/// capture); on a mixed-DPI setup the canvas uses the sharpest display's scale
/// and 1x panels are upscaled onto it, so no display is ever downscaled.
#[must_use]
pub fn canvas_scale(shots: &[DisplayShot]) -> f64 {
    shots
        .iter()
        .map(|s| {
            pixel_scale(
                usize::try_from(s.pixels_w).unwrap_or(usize::MAX),
                s.bounds.width,
            )
        })
        .fold(1.0, f64::max)
}

/// Pixel dimensions of the canvas for `bounds` (display points) at `scale`.
///
/// # Errors
/// [`DesktopError::Screenshot`] when the canvas is empty, non-finite or too
/// large for a `u32`-sized buffer.
pub fn canvas_size(bounds: &Rect, scale: f64) -> Result<(u32, u32), DesktopError> {
    let err = || {
        DesktopError::Screenshot(
            "the stitched display canvas is empty, malformed or too large".into(),
        )
    };
    // `clamp` bounds every value to [0, u32::MAX] (a NaN rounds and casts to 0
    // and is rejected by the zero check below).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // values are rounded and clamped to [0, u32::MAX] first
    let as_u32 = |v: f64| v.clamp(0.0, f64::from(u32::MAX)) as u32;
    let w = as_u32((bounds.width * scale).round());
    let h = as_u32((bounds.height * scale).round());
    if w == 0 || h == 0 {
        return Err(err());
    }
    Ok((w, h))
}

/// Stitch every display onto one canvas of [`virtual_bounds`] at [`canvas_scale`].
///
/// The canvas is filled opaque black (a gap between displays must read as
/// black, not as a transparent pixel), then each shot is blitted at its
/// bounds. Source pixels are sampled nearest-neighbour at
/// `display_pixels / canvas_pixels` (no third-party image crate; deterministic
/// and unit-testable). Mirrored displays (identical bounds) overlap: the later
/// shot wins, which is correct because mirrored screens show the same content.
///
/// # Errors
/// [`DesktopError::Screenshot`] when the canvas is malformed or a shot is
/// malformed (non-finite or empty bounds, empty pixel buffer, or a buffer that
/// does not match its pixel dimensions).
pub fn stitch(shots: &[DisplayShot], bounds: &Rect, scale: f64) -> Result<Vec<u8>, DesktopError> {
    let (canvas_w, canvas_h) = canvas_size(bounds, scale)?;
    let mut canvas = black_canvas(canvas_w, canvas_h);
    for shot in shots {
        blit(&mut canvas, canvas_w, canvas_h, bounds, scale, shot)?;
    }
    Ok(canvas)
}

/// An opaque black canvas of `width` × `height` RGBA-8 pixels.
fn black_canvas(width: u32, height: u32) -> Vec<u8> {
    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    for px in canvas.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[0, 0, 0, 255]);
    }
    canvas
}

/// Place one captured display onto the canvas.
///
/// # Errors
/// [`DesktopError::Screenshot`] when the shot is malformed: non-finite or
/// non-positive bounds, a zero pixel dimension, or a buffer that does not hold
/// exactly `pixels_w * pixels_h * 4` bytes.
fn blit(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    origin: &Rect,
    scale: f64,
    shot: &DisplayShot,
) -> Result<(), DesktopError> {
    let err = || DesktopError::Screenshot("a captured display is malformed".into());
    let bounds = &shot.bounds;
    if !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
        || bounds.width <= 0.0
        || bounds.height <= 0.0
        || shot.pixels_w == 0
        || shot.pixels_h == 0
    {
        return Err(err());
    }
    let buffer_len = u64::from(shot.pixels_w) * u64::from(shot.pixels_h) * 4;
    if buffer_len != shot.rgba.len() as u64 {
        return Err(err());
    }

    // Destination rectangle in canvas pixels. `origin` is the canvas origin in
    // display points, so the offsets are non-negative up to rounding; clamp to
    // the canvas and drop a degenerate sliver instead of erroring (a display
    // smaller than one canvas pixel cannot exist on a real setup).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // values are rounded and clamped to [0, u32::MAX] first
    let as_u32 = |v: f64| v.clamp(0.0, f64::from(u32::MAX)) as u32;
    let dst_x = as_u32(((bounds.x - origin.x).max(0.0) * scale).round());
    let dst_y = as_u32(((bounds.y - origin.y).max(0.0) * scale).round());
    let dst_w = as_u32((bounds.width * scale).round());
    let dst_h = as_u32((bounds.height * scale).round());
    if dst_w == 0 || dst_h == 0 {
        return Err(err());
    }
    let right = dst_x.saturating_add(dst_w).min(canvas_w);
    let bottom = dst_y.saturating_add(dst_h).min(canvas_h);

    for y in dst_y..bottom {
        // Nearest-neighbour sample: `source / canvas` ratio; the product
        // needs u64, the quotient stays below `pixels_h` (a u32).
        let src_y =
            u32::try_from(u64::from(y - dst_y) * u64::from(shot.pixels_h) / u64::from(dst_h))
                .unwrap_or(u32::MAX)
                .min(shot.pixels_h - 1);
        let src_row = src_y as usize * shot.pixels_w as usize * 4;
        let dst_row = y as usize * canvas_w as usize * 4;
        for x in dst_x..right {
            let src_x =
                u32::try_from(u64::from(x - dst_x) * u64::from(shot.pixels_w) / u64::from(dst_w))
                    .unwrap_or(u32::MAX)
                    .min(shot.pixels_w - 1);
            let src = src_row + src_x as usize * 4;
            let dst = dst_row + x as usize * 4;
            canvas[dst..dst + 4].copy_from_slice(&shot.rgba[src..src + 4]);
        }
    }
    Ok(())
}

/// Encode RGBA pixels as JPEG (lossy qualities) or PNG ([`Quality::Lossless`]).
///
/// # Errors
/// [`DesktopError::Screenshot`] when the pixels do not fit the encoder or the
/// encoder fails.
pub fn encode_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: Quality,
) -> Result<Image, DesktopError> {
    let mut out = Vec::new();
    let mime_type = if quality == Quality::Lossless {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        writer
            .finish()
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        "image/png"
    } else {
        let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(DesktopError::Screenshot("image too large for JPEG".into()));
        };
        jpeg_encoder::Encoder::new(&mut out, jpeg_quality(quality))
            .encode(rgba, w, h, jpeg_encoder::ColorType::Rgba)
            .map_err(|e| DesktopError::Screenshot(format!("jpeg: {e}")))?;
        "image/jpeg"
    };
    Ok(Image {
        data: out,
        width,
        height,
        mime_type: mime_type.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_quality_matches_the_windows_backend() {
        assert_eq!(jpeg_quality(Quality::Low), 40);
        assert_eq!(jpeg_quality(Quality::Medium), 70);
        assert_eq!(jpeg_quality(Quality::High), 90);
        assert_eq!(jpeg_quality(Quality::Lossless), 90);
    }

    #[test]
    fn row_trimming_removes_stride_padding_and_extra_rows() {
        // Two rows of 2 px (8 bytes) with 4 bytes of padding after each row,
        // plus the stray third row CoreGraphics sometimes hands back.
        let data: Vec<u8> = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, // row 0 + padding
            9, 10, 11, 12, 13, 14, 15, 16, 0xBB, 0xBB, 0xBB, 0xBB, // row 1 + padding
            0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xDD, 0xDD, 0xDD,
            0xDD, // extra row
        ]
        .to_vec();
        let trimmed = trim_bgra_rows(&data, 12, 2, 2);
        assert_eq!(
            trimmed,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn row_trimming_keeps_unpadded_data() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(trim_bgra_rows(&data, 8, 2, 1), data);
    }

    #[test]
    fn bgra_becomes_opaque_rgba() {
        let mut px = [0x01, 0x02, 0x03, 0x00];
        bgra_to_rgba(&mut px);
        assert_eq!(px, [0x03, 0x02, 0x01, 0xFF]);
    }

    #[test]
    fn scale_is_at_least_one_and_guards_zero_points() {
        assert!((pixel_scale(2940, 1470.0) - 2.0).abs() < 1e-9);
        assert!((pixel_scale(100, 0.0) - 1.0).abs() < 1e-9);
        assert!((pixel_scale(100, -5.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cropping_scales_points_to_pixels() {
        let rgba = vec![1; 8 * 8 * 4];
        let rect = Rect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 2.0,
        };
        let (px, w, h) = crop_rect(&rgba, 8, 8, &rect, 2.0).expect("crop");
        assert_eq!((w, h), (6, 4));
        assert_eq!(px.len(), 6 * 4 * 4);
    }

    #[test]
    fn cropping_clamps_out_of_bounds_rects() {
        let rgba = vec![1; 8 * 8 * 4];
        let rect = Rect {
            x: 4.0,
            y: 4.0,
            width: 100.0,
            height: 100.0,
        };
        let (px, w, h) = crop_rect(&rgba, 8, 8, &rect, 1.0).expect("crop");
        assert_eq!((w, h), (4, 4));
        assert_eq!(px.len(), 4 * 4 * 4);
    }

    #[test]
    fn an_outside_or_empty_rect_is_an_error() {
        let rgba = vec![1; 8 * 8 * 4];
        let outside = Rect {
            x: 9.0,
            y: 9.0,
            width: 2.0,
            height: 2.0,
        };
        assert!(crop_rect(&rgba, 8, 8, &outside, 1.0).is_err());
        let zero = Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        assert!(crop_rect(&rgba, 8, 8, &zero, 1.0).is_err());
    }

    #[test]
    fn transparency_detection() {
        assert!(is_fully_transparent(&[0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!is_fully_transparent(&[0, 0, 0, 0, 1, 2, 3, 255]));
    }

    #[test]
    fn lossless_encoding_produces_png() {
        let rgba = vec![128; 2 * 2 * 4];
        let img = encode_rgba(&rgba, 2, 2, Quality::Lossless).expect("encode");
        assert_eq!(img.mime_type, "image/png");
        assert_eq!((img.width, img.height), (2, 2));
        assert!(
            img.data
                .starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        );
    }

    #[test]
    fn lossy_encoding_produces_jpeg() {
        let rgba = vec![128; 2 * 2 * 4];
        let img = encode_rgba(&rgba, 2, 2, Quality::Low).expect("encode");
        assert_eq!(img.mime_type, "image/jpeg");
        assert!(img.data.starts_with(&[0xFF, 0xD8]));
    }

    /// A solid-colour [`DisplayShot`]: every pixel is `rgba`.
    fn solid(bounds: Rect, pixels_w: u32, pixels_h: u32, rgba: [u8; 4]) -> DisplayShot {
        DisplayShot {
            bounds,
            pixels_w,
            pixels_h,
            rgba: std::iter::repeat_n(rgba, (pixels_w * pixels_h) as usize)
                .flatten()
                .collect(),
        }
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn side_by_side_displays_stitch_into_a_wider_canvas() {
        let shots = [
            solid(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                RED,
            ),
            solid(
                Rect {
                    x: 100.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                BLUE,
            ),
        ];
        let bounds = virtual_bounds(&shots).expect("bounds");
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (0.0, 0.0, 200.0, 100.0)
        );
        let scale = canvas_scale(&shots);
        assert!((scale - 1.0).abs() < 1e-9);
        let canvas = stitch(&shots, &bounds, scale).expect("stitch");
        let (w, h) = canvas_size(&bounds, scale).expect("size");
        assert_eq!((w, h), (200, 100));
        let px = |x: u32, y: u32| -> [u8; 4] {
            let at = (y as usize * w as usize + x as usize) * 4;
            canvas[at..at + 4].try_into().expect("pixel")
        };
        assert_eq!(px(0, 0), RED);
        assert_eq!(px(99, 50), RED);
        assert_eq!(px(100, 0), BLUE);
        assert_eq!(px(199, 99), BLUE);
    }

    #[test]
    fn a_display_left_of_the_main_one_gets_negative_origins() {
        let shots = [
            solid(
                Rect {
                    x: -100.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                RED,
            ),
            solid(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                BLUE,
            ),
        ];
        let bounds = virtual_bounds(&shots).expect("bounds");
        assert_eq!((bounds.x, bounds.width), (-100.0, 200.0));
        let canvas = stitch(&shots, &bounds, 1.0).expect("stitch");
        let px = |x: u32, y: u32| -> [u8; 4] {
            let at = (y as usize * 200 + x as usize) * 4;
            canvas[at..at + 4].try_into().expect("pixel")
        };
        // The left half of the canvas is the left-of-main display, red.
        assert_eq!(px(0, 0), RED);
        assert_eq!(px(99, 99), RED);
        assert_eq!(px(100, 50), BLUE);
    }

    #[test]
    fn mixed_dpi_uses_the_highest_scale_and_upscales_lower_dpi_panels() {
        let shots = [
            // A 2x display: 100x100 points captured as 200x200 pixels.
            solid(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                200,
                200,
                RED,
            ),
            // A 1x display of the same point size next to it.
            solid(
                Rect {
                    x: 100.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                BLUE,
            ),
        ];
        let bounds = virtual_bounds(&shots).expect("bounds");
        let scale = canvas_scale(&shots);
        assert!((scale - 2.0).abs() < 1e-9);
        let canvas = stitch(&shots, &bounds, scale).expect("stitch");
        let (w, h) = canvas_size(&bounds, scale).expect("size");
        assert_eq!((w, h), (400, 200));
        let px = |x: u32, y: u32| -> [u8; 4] {
            let at = (y as usize * w as usize + x as usize) * 4;
            canvas[at..at + 4].try_into().expect("pixel")
        };
        assert_eq!(px(0, 0), RED);
        assert_eq!(px(399, 0), BLUE);
        // The 1x panel is upscaled (every source pixel doubles), so its centre
        // and right edge are still its own colour.
        assert_eq!(px(200, 100), BLUE);
        assert_eq!(px(399, 199), BLUE);
    }

    #[test]
    fn a_single_2x_display_loses_no_resolution() {
        let shots = [solid(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            200,
            200,
            RED,
        )];
        let bounds = virtual_bounds(&shots).expect("bounds");
        let scale = canvas_scale(&shots);
        let (w, h) = canvas_size(&bounds, scale).expect("size");
        assert_eq!((w, h), (200, 200));
        let canvas = stitch(&shots, &bounds, scale).expect("stitch");
        assert_eq!(canvas.len(), 200 * 200 * 4);
        assert!(canvas.as_chunks::<4>().0.iter().all(|px| *px == RED));
    }

    #[test]
    fn mirrored_displays_overlap_and_the_later_one_wins() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let shots = [solid(bounds, 100, 100, RED), solid(bounds, 100, 100, BLUE)];
        let canvas = stitch(&shots, &bounds, 1.0).expect("stitch");
        assert!(canvas.as_chunks::<4>().0.iter().all(|px| *px == BLUE));
    }

    #[test]
    fn no_displays_yield_no_canvas_and_an_empty_bounds_is_an_error() {
        assert!(virtual_bounds(&[]).is_none());
        let zero = Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        assert!(canvas_size(&zero, 1.0).is_err());
        // An empty shot list over valid bounds is simply an all-black canvas.
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 2.0,
            height: 2.0,
        };
        let canvas = stitch(&[], &bounds, 1.0).expect("stitch");
        assert!(
            canvas
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [0, 0, 0, 255])
        );
    }

    #[test]
    fn a_pixel_buffer_that_does_not_match_its_dimensions_is_an_error() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 2.0,
            height: 2.0,
        };
        let short = DisplayShot {
            bounds,
            pixels_w: 2,
            pixels_h: 2,
            rgba: vec![0; 15 * 4],
        };
        assert!(stitch(&[short], &bounds, 1.0).is_err());
        let empty = DisplayShot {
            bounds,
            pixels_w: 0,
            pixels_h: 2,
            rgba: Vec::new(),
        };
        assert!(stitch(&[empty], &bounds, 1.0).is_err());
        let nan = DisplayShot {
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: f64::NAN,
                height: 2.0,
            },
            pixels_w: 2,
            pixels_h: 2,
            rgba: vec![0; 16 * 4],
        };
        assert!(stitch(&[nan], &bounds, 1.0).is_err());
    }

    #[test]
    fn a_crop_rect_may_span_two_displays() {
        let shots = [
            solid(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                RED,
            ),
            solid(
                Rect {
                    x: 100.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                100,
                100,
                BLUE,
            ),
        ];
        let bounds = virtual_bounds(&shots).expect("bounds");
        let canvas = stitch(&shots, &bounds, 1.0).expect("stitch");
        let span = Rect {
            x: 90.0,
            y: 40.0,
            width: 20.0,
            height: 10.0,
        };
        let (px, w, h) = crop_rect(&canvas, 200, 100, &span, 1.0).expect("crop");
        assert_eq!((w, h), (20, 10));
        // Left edge is red (first display), right edge is blue (second).
        assert_eq!(&px[0..4], &RED);
        assert_eq!(&px[(20 - 1) * 4..20 * 4], &BLUE);
    }
}
