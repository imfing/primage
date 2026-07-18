//! Command-line interface definition.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Parser, ValueEnum};

use crate::codecs::Format;

/// Compress and convert images like Squoosh — no browser required.
///
/// Decodes JPEG, PNG, WebP, GIF, TIFF, BMP, ICO, TGA, PNM and QOI,
/// and encodes to JPEG, PNG (OxiPNG), WebP (lossless), AVIF and QOI.
#[derive(Parser, Debug)]
#[command(name = "primage", version, about, long_about)]
pub struct Cli {
    /// Input image file(s)
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    /// Output file, or output directory when processing multiple inputs
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Output format (default: same as the input)
    #[arg(short, long, value_enum, value_name = "FORMAT")]
    pub format: Option<Format>,

    /// Quality for lossy encoders (codec defaults: jpeg=75, avif=50)
    #[arg(short, long, value_name = "1-100", value_parser = clap::value_parser!(u8).range(1..=100))]
    pub quality: Option<u8>,

    /// Resize before encoding: WxH, Wx (auto height) or xH (auto width)
    #[arg(long, value_name = "GEOMETRY")]
    pub resize: Option<Resize>,

    /// Rotate before encoding
    #[arg(long, value_enum)]
    pub rotate: Option<Rotation>,

    /// Resampling filter used with --resize
    #[arg(long, value_enum, default_value_t = ResizeFilter::Lanczos3)]
    pub resize_filter: ResizeFilter,

    /// OxiPNG optimization level: 0 (fast) .. 6 (slow, smallest)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=6))]
    pub png_level: u8,

    /// Write interlaced (Adam7) PNGs
    #[arg(long)]
    pub png_interlace: bool,

    /// AVIF encoder speed: 0 (slow, best) .. 10 (fast)
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(0..=10))]
    pub avif_speed: u8,

    /// Suffix appended to generated file names, e.g. -s .min
    #[arg(short, long, default_value = "")]
    pub suffix: String,

    /// Allow overwriting the input file
    #[arg(long)]
    pub overwrite: bool,
}

/// Resize geometry: `WxH`, `Wx` (auto height) or `xH` (auto width).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resize {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl FromStr for Resize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (w, h) = s
            .split_once(['x', 'X'])
            .ok_or_else(|| "expected WxH, Wx or xH".to_string())?;
        let parse = |v: &str| match v.trim() {
            "" => Ok(None),
            v => v
                .parse::<u32>()
                .map(Some)
                .map_err(|_| format!("invalid dimension: {v:?}")),
        };
        let (width, height) = (parse(w)?, parse(h)?);
        if width.is_none() && height.is_none() {
            return Err("at least one dimension is required".into());
        }
        if width == Some(0) || height == Some(0) {
            return Err("dimensions must be greater than zero".into());
        }
        Ok(Self { width, height })
    }
}

/// Rotation applied before encoding, like Squoosh's rotate preprocessor.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Rotation {
    #[value(name = "90")]
    R90,
    #[value(name = "180")]
    R180,
    #[value(name = "270")]
    R270,
}

/// Resampling filters, mirroring Squoosh's resize methods.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ResizeFilter {
    Triangle,
    Catrom,
    Gaussian,
    #[default]
    Lanczos3,
    Nearest,
}

impl From<ResizeFilter> for image::imageops::FilterType {
    fn from(filter: ResizeFilter) -> Self {
        use image::imageops::FilterType as F;
        match filter {
            ResizeFilter::Triangle => F::Triangle,
            ResizeFilter::Catrom => F::CatmullRom,
            ResizeFilter::Gaussian => F::Gaussian,
            ResizeFilter::Lanczos3 => F::Lanczos3,
            ResizeFilter::Nearest => F::Nearest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resize_geometry() {
        assert_eq!(
            "800x600".parse(),
            Ok(Resize {
                width: Some(800),
                height: Some(600)
            })
        );
        assert_eq!(
            "800x".parse(),
            Ok(Resize {
                width: Some(800),
                height: None
            })
        );
        assert_eq!(
            "x600".parse(),
            Ok(Resize {
                width: None,
                height: Some(600)
            })
        );
        assert!("x".parse::<Resize>().is_err());
        assert!("0x600".parse::<Resize>().is_err());
        assert!("800".parse::<Resize>().is_err());
        assert!("800x600x1".parse::<Resize>().is_err());
    }
}
