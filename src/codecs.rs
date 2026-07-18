//! Output codecs and their backends.
//!
//! | Format | Default backend                                  |
//! |--------|--------------------------------------------------|
//! | JPEG   | `mozjpeg` crate (pure-Rust baseline fallback)    |
//! | PNG    | `oxipng`                                         |
//! | WebP   | `webp` / libwebp (`image-webp` lossless fallback)|
//! | AVIF   | `ravif` + rav1e (pure Rust)                      |
//! | QOI    | `image` crate                                    |
//!
//! The C codecs (mozjpeg, libwebp) are statically linked into the binary.

use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

/// Output image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// JPEG — MozJPEG with the `mozjpeg` feature (default), pure-Rust baseline otherwise
    #[value(alias = "jpg")]
    Jpeg,
    /// PNG, optimized with OxiPNG
    Png,
    /// WebP — lossy via libwebp (default), or lossless with --lossless
    Webp,
    /// AVIF, encoded with ravif/rav1e
    Avif,
    /// QOI — the "Quite OK Image" format
    Qoi,
}

impl Format {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            "avif" => Some(Self::Avif),
            "qoi" => Some(Self::Qoi),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Qoi => "qoi",
        }
    }
}

/// Per-codec encoding options.
pub struct EncodeOptions {
    /// Lossy quality 1–100 (defaults: jpeg=75, webp=75, avif=50).
    pub quality: Option<u8>,
    /// Lossless WebP compression.
    pub lossless: bool,
    /// OxiPNG optimization preset 0–6 (default: 2).
    pub png_level: u8,
    /// Interlace (Adam7) PNG output (default: false).
    pub png_interlace: bool,
    /// AVIF encoder speed 0–10 (default: 6).
    #[cfg_attr(not(feature = "avif"), allow(dead_code))]
    pub avif_speed: u8,
}

pub fn encode(img: &RgbaImage, format: Format, opts: &EncodeOptions) -> Result<Vec<u8>> {
    match format {
        Format::Jpeg => encode_jpeg(img, opts),
        Format::Png => encode_png(img, opts),
        Format::Webp => encode_webp(img, opts),
        Format::Avif => encode_avif(img, opts),
        Format::Qoi => encode_qoi(img),
    }
}

/// Real MozJPEG. Defaults: progressive scans, optimized Huffman coding,
/// 4:2:0 chroma subsampling (4:4:4 at quality ≥ 90, mozjpeg's
/// auto_subsample behavior).
#[cfg(feature = "mozjpeg")]
fn encode_jpeg(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let quality = opts.quality.unwrap_or(75);
    let rgb = flatten_alpha(img);

    let mut comp = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
    comp.set_size(img.width() as usize, img.height() as usize);
    comp.set_quality(quality as f32);
    comp.set_progressive_mode();
    comp.set_optimize_coding(true);
    let sampling = if quality >= 90 { (1, 1) } else { (2, 2) };
    comp.set_chroma_sampling_pixel_sizes(sampling, sampling);

    let mut started = comp
        .start_compress(Vec::new())
        .context("mozjpeg: failed to start compressor")?;
    started
        .write_scanlines(&rgb)
        .context("mozjpeg: encode failed")?;
    started.finish().context("mozjpeg: finish failed")
}

/// Pure-Rust baseline JPEG from the `image` crate. Progressive encoding and
/// trellis optimization don't exist in pure Rust yet — build with
/// `--features mozjpeg` for those.
#[cfg(not(feature = "mozjpeg"))]
fn encode_jpeg(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let quality = opts.quality.unwrap_or(75);
    let rgb = flatten_alpha(img);

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).write_image(
        &rgb,
        img.width(),
        img.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// JPEG has no alpha channel — composite onto white first.
fn flatten_alpha(img: &RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((img.width() * img.height() * 3) as usize);
    for p in img.pixels() {
        let [r, g, b, a] = p.0;
        if a == 255 {
            rgb.extend_from_slice(&[r, g, b]);
        } else {
            let a = u16::from(a);
            let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
            rgb.extend_from_slice(&[blend(r), blend(g), blend(b)]);
        }
    }
    rgb
}

/// PNG: fast initial encode, then OxiPNG optimization.
fn encode_png(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};

    let mut raw = Vec::new();
    PngEncoder::new_with_quality(&mut raw, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )?;

    let mut options = oxipng::Options::from_preset(opts.png_level);
    options.interlace = Some(opts.png_interlace);
    oxipng::optimize_from_memory(&raw, &options).context("oxipng optimization failed")
}

/// WebP via libwebp, statically linked. Lossy at quality 75 by default
/// (libwebp's default method 4), lossless with `--lossless`.
#[cfg(feature = "libwebp")]
fn encode_webp(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let encoder = webp::Encoder::from_rgba(img.as_raw(), img.width(), img.height());
    let encoded = if opts.lossless {
        encoder.encode_lossless()
    } else {
        let quality = opts.quality.unwrap_or(75);
        encoder.encode(f32::from(quality))
    };
    Ok(encoded.to_vec())
}

/// Pure-Rust WebP fallback: lossless only, via `image-webp`. A pure-Rust
/// lossy (VP8) encoder doesn't exist yet.
#[cfg(not(feature = "libwebp"))]
fn encode_webp(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    if !opts.lossless {
        eprintln!("warning: lossy WebP requires the `libwebp` feature; encoding lossless instead");
    }
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image_webp::ColorType::Rgba8,
        )
        .context("WebP encode failed")?;
    Ok(out)
}

/// AVIF via ravif (pure-Rust frontend around rav1e).
/// Defaults: quality 50, speed 6.
#[cfg(feature = "avif")]
fn encode_avif(img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let quality = opts.quality.unwrap_or(50);
    let pixels: Vec<ravif::RGBA8> = img
        .as_raw()
        .chunks_exact(4)
        .map(|p| ravif::RGBA8 {
            r: p[0],
            g: p[1],
            b: p[2],
            a: p[3],
        })
        .collect();
    let encoded = ravif::Encoder::new()
        .with_quality(f32::from(quality))
        .with_speed(opts.avif_speed)
        .encode_rgba(ravif::Img::new(
            &pixels,
            img.width() as usize,
            img.height() as usize,
        ))
        .map_err(|e| anyhow::anyhow!("AVIF encode failed: {e}"))?;
    Ok(encoded.avif_file)
}

#[cfg(not(feature = "avif"))]
fn encode_avif(_: &RgbaImage, _: &EncodeOptions) -> Result<Vec<u8>> {
    anyhow::bail!("this build was compiled without AVIF support (enable the `avif` feature)")
}

/// QOI — no options, it's always "lossless + fast".
fn encode_qoi(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::qoi::QoiEncoder::new(&mut out).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgba8,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image() -> RgbaImage {
        RgbaImage::from_fn(32, 24, |x, y| {
            image::Rgba([(x * 8) as u8, (y * 8) as u8, 128, 255])
        })
    }

    fn roundtrip(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).expect("output should decode");
        (img.width(), img.height())
    }

    fn opts() -> EncodeOptions {
        EncodeOptions {
            quality: None,
            lossless: false,
            png_level: 2,
            png_interlace: false,
            avif_speed: 6,
        }
    }

    #[test]
    fn jpeg_roundtrip() {
        let bytes = encode(&test_image(), Format::Jpeg, &opts()).unwrap();
        assert_eq!(roundtrip(&bytes), (32, 24));
    }

    #[test]
    fn png_roundtrip() {
        let bytes = encode(&test_image(), Format::Png, &opts()).unwrap();
        assert_eq!(roundtrip(&bytes), (32, 24));
    }

    #[test]
    fn webp_roundtrip() {
        let bytes = encode(&test_image(), Format::Webp, &opts()).unwrap();
        assert_eq!(roundtrip(&bytes), (32, 24));
    }

    #[test]
    fn webp_lossless_roundtrip() {
        let opts = EncodeOptions {
            lossless: true,
            ..opts()
        };
        let bytes = encode(&test_image(), Format::Webp, &opts).unwrap();
        assert_eq!(roundtrip(&bytes), (32, 24));
    }

    #[test]
    fn qoi_roundtrip() {
        let bytes = encode(&test_image(), Format::Qoi, &opts()).unwrap();
        assert_eq!(roundtrip(&bytes), (32, 24));
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_encodes() {
        let bytes = encode(&test_image(), Format::Avif, &opts()).unwrap();
        assert!(bytes.len() > 12 && &bytes[4..12] == b"ftypavif");
    }
}
