//! Preprocessors: rotate and resize.

use image::{imageops, RgbaImage};

use crate::cli::Rotation;

pub fn rotate(img: &RgbaImage, rotation: Rotation) -> RgbaImage {
    match rotation {
        Rotation::R90 => imageops::rotate90(img),
        Rotation::R180 => imageops::rotate180(img),
        Rotation::R270 => imageops::rotate270(img),
    }
}

pub fn resize(
    img: &RgbaImage,
    width: Option<u32>,
    height: Option<u32>,
    filter: imageops::FilterType,
) -> RgbaImage {
    let (w, h) = resolve_dimensions(img.width(), img.height(), width, height);
    if (w, h) == (img.width(), img.height()) {
        return img.clone();
    }
    imageops::resize(img, w, h, filter)
}

/// Shrink so the longest side is at most `max`, preserving aspect ratio.
/// No-op if the image already fits.
pub fn fit_within(img: &RgbaImage, max: u32, filter: imageops::FilterType) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let scale = f64::from(max) / f64::from(w.max(h));
    if scale >= 1.0 {
        return img.clone();
    }
    let (nw, nh) = (
        (w as f64 * scale).round().max(1.0) as u32,
        (h as f64 * scale).round().max(1.0) as u32,
    );
    imageops::resize(img, nw, nh, filter)
}

/// Resolve target dimensions, preserving aspect ratio when one side is unset.
fn resolve_dimensions(
    orig_w: u32,
    orig_h: u32,
    width: Option<u32>,
    height: Option<u32>,
) -> (u32, u32) {
    let scale = |a: u32, b: u32, new_a: u32| {
        // new_b = b * (new_a / a), rounded to nearest, clamped to >= 1
        ((u64::from(b) * u64::from(new_a) + u64::from(a) / 2) / u64::from(a)).max(1) as u32
    };
    match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, scale(orig_w, orig_h, w)),
        (None, Some(h)) => (scale(orig_h, orig_w, h), h),
        (None, None) => (orig_w, orig_h),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_dimensions;

    #[test]
    fn resolve_dims() {
        assert_eq!(resolve_dimensions(1920, 1080, Some(960), Some(540)), (960, 540));
        assert_eq!(resolve_dimensions(1920, 1080, Some(960), None), (960, 540));
        assert_eq!(resolve_dimensions(1920, 1080, None, Some(540)), (960, 540));
        assert_eq!(resolve_dimensions(100, 100, None, None), (100, 100));
        // Odd aspect ratios round to nearest and never hit zero.
        assert_eq!(resolve_dimensions(3, 1, Some(1), None), (1, 1));
        assert_eq!(resolve_dimensions(4000, 3, Some(1000), None), (1000, 1));
    }
}
