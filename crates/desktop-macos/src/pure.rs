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
}
