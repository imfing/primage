//! The per-file pipeline: decode → preprocess → encode → write.
//!
//! Split into two phases so batches can be planned serially (output-name
//! resolution and deduplication) and then executed in parallel with rayon.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::ImageReader;

use crate::cli::Cli;
use crate::codecs::{self, Format};
use crate::transform;

/// A fully resolved conversion job. `output`/`format` are `None` in
/// preview-only mode (nothing is written).
pub struct Plan {
    pub input: PathBuf,
    output: Option<PathBuf>,
    format: Option<Format>,
}

/// Result of processing one input file.
pub struct Report {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub input_size: u64,
    pub output_size: Option<u64>,
    /// Original pixel dimensions, after any rotate/resize.
    pub dimensions: (u32, u32),
    /// Downscaled copy for terminal display, when --preview is set.
    pub image: Option<image::RgbaImage>,
    /// Explanation when an encoded output cannot be previewed.
    pub preview_warning: Option<&'static str>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.output, self.output_size) {
            (Some(output), Some(output_size)) => {
                let change = size_change(self.input_size, output_size);
                write!(
                    f,
                    "{} → {}  {} → {}  ({change:+.1}%)",
                    self.input.display(),
                    output.display(),
                    human_bytes(self.input_size),
                    human_bytes(output_size),
                )
            }
            _ => write!(
                f,
                "{}  {}×{}, {}",
                self.input.display(),
                self.dimensions.0,
                self.dimensions.1,
                human_bytes(self.input_size),
            ),
        }
    }
}

/// Percentage change from input to output size (negative = smaller).
fn size_change(input: u64, output: u64) -> f64 {
    if input == 0 {
        return 0.0;
    }
    (output as f64 / input as f64 - 1.0) * 100.0
}

/// Resolve the output format and path for one input, without touching the image.
pub fn plan(input: &Path, args: &Cli, taken: &mut HashSet<PathBuf>) -> Result<Plan> {
    if args.preview_only() {
        return Ok(Plan {
            input: input.to_path_buf(),
            output: None,
            format: None,
        });
    }
    let format = resolve_format(input, args)?;
    let output = resolve_output_path(input, args, format, taken)?;
    if output == input && !args.overwrite {
        bail!("output would overwrite the input file; pass --overwrite, --suffix or -o");
    }
    Ok(Plan {
        input: input.to_path_buf(),
        output: Some(output),
        format: Some(format),
    })
}

/// Execute a plan: decode, preprocess, encode, write.
pub fn run(plan: &Plan, args: &Cli, preview_pixel_width: Option<u32>) -> Result<Report> {
    let input_size = std::fs::metadata(&plan.input)
        .with_context(|| format!("cannot read {}", plan.input.display()))?
        .len();

    let img = ImageReader::open(&plan.input)
        .with_context(|| format!("cannot open {}", plan.input.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode {}", plan.input.display()))?
        .to_rgba8();

    // Preprocessors: rotate, then resize.
    let img = match args.rotate {
        Some(rotation) => transform::rotate(&img, rotation),
        None => img,
    };
    let img = match (args.resize, args.max_size) {
        (Some(geometry), _) => transform::resize(
            &img,
            geometry.width,
            geometry.height,
            args.resize_filter.into(),
        ),
        (None, Some(max)) => transform::fit_within(&img, max, args.resize_filter.into()),
        (None, None) => img,
    };

    let dimensions = (img.width(), img.height());
    let mut encoded_preview = None;
    let mut preview_warning = None;

    let output_size = match (plan.output.as_ref(), plan.format) {
        (Some(output), Some(format)) => {
            let opts = codecs::EncodeOptions {
                quality: args.quality,
                lossless: args.lossless,
                png_level: args.png_level.unwrap_or(2),
                png_interlace: args.png_interlace,
                avif_speed: args.avif_speed.unwrap_or(6),
            };
            let bytes = codecs::encode(&img, format, &opts)?;

            if args.preview && preview_pixel_width.is_some() {
                match decode_encoded_preview(&bytes, format)? {
                    Some(image) => encoded_preview = Some(image),
                    None => {
                        preview_warning = Some(
                            "encoded AVIF preview is unavailable because AVIF decoding is not \
                             included; the output file was still written",
                        );
                    }
                }
            }

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(output, &bytes)
                .with_context(|| format!("cannot write {}", output.display()))?;
            Some(bytes.len() as u64)
        }
        _ => None,
    };

    // Resize exactly once to the terminal's pixel width. The terminal can then
    // display the pixels natively instead of resampling a cell-sized placement.
    let image = preview_pixel_width
        .filter(|_| args.preview)
        .and_then(|width| {
            encoded_preview
                .as_ref()
                .or_else(|| plan.output.is_none().then_some(&img))
                .map(|source| {
                    transform::fit_width(source, width, image::imageops::FilterType::Lanczos3)
                })
        });

    Ok(Report {
        input: plan.input.clone(),
        output: plan.output.clone(),
        input_size,
        output_size,
        dimensions,
        image,
        preview_warning,
    })
}

/// Decode the bytes that were actually written so lossy artifacts are visible
/// in the terminal preview. AVIF output is the only exception because this
/// project does not currently include an AVIF decoder.
fn decode_encoded_preview(bytes: &[u8], format: Format) -> Result<Option<image::RgbaImage>> {
    if format == Format::Avif {
        return Ok(None);
    }
    image::load_from_memory(bytes)
        .context("failed to decode encoded output for preview")
        .map(|image| Some(image.to_rgba8()))
}

/// Pick the output format: explicit flag, then the output path's extension,
/// then the input's extension.
fn resolve_format(input: &Path, args: &Cli) -> Result<Format> {
    if let Some(format) = args.format {
        return Ok(format);
    }
    if let Some(out) = &args.output {
        let dir_target = args.inputs.len() > 1 || out.is_dir();
        if !dir_target {
            if let Some(format) = Format::from_path(out) {
                return Ok(format);
            }
        }
    }
    if let Some(format) = Format::from_path(input) {
        return Ok(format);
    }
    bail!(
        "cannot infer an output format from {}; specify --format",
        input.display()
    )
}

/// Compute the output path for one input, deduplicating names generated
/// within this run (e.g. `a.png` and `a.webp` both mapping to `a.jpg`).
fn resolve_output_path(
    input: &Path,
    args: &Cli,
    format: Format,
    taken: &mut HashSet<PathBuf>,
) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .with_context(|| format!("invalid file name: {}", input.display()))?
        .to_string_lossy();
    let ext = format.extension();
    let default_same_format_number = args.output.is_none()
        && args.suffix.is_empty()
        && !args.overwrite
        && Format::from_path(input) == Some(format);
    let auto_name = |n: Option<u32>| {
        if default_same_format_number {
            return format!("{stem}{}.{ext}", n.unwrap_or(1));
        }
        let counter = n.map(|n| format!("-{n}")).unwrap_or_default();
        format!("{stem}{}{counter}.{ext}", args.suffix)
    };

    let generated_name = match &args.output {
        Some(out) => args.inputs.len() > 1 || out.is_dir(),
        None => true,
    };
    let mut candidate = match &args.output {
        // Multiple inputs, or an existing directory: treat -o as a directory.
        Some(out) if args.inputs.len() > 1 || out.is_dir() => out.join(auto_name(None)),
        Some(out) => out.clone(),
        None => input.with_file_name(auto_name(None)),
    };
    let mut n = 2;
    while taken.contains(&candidate) || (generated_name && candidate.exists()) {
        candidate = match &args.output {
            Some(out) if args.inputs.len() > 1 || out.is_dir() => out.join(auto_name(Some(n))),
            Some(out) => out.with_file_name(auto_name(Some(n))),
            None => input.with_file_name(auto_name(Some(n))),
        };
        n += 1;
    }
    taken.insert(candidate.clone());
    Ok(candidate)
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut size, mut unit) = (n as f64, 0);
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use image::{Rgba, RgbaImage};

    fn args(values: &[&str]) -> Cli {
        Cli::try_parse_from(values).unwrap()
    }

    #[test]
    fn same_format_uses_a_safe_default_number() {
        let args = args(&["primage", "photo.jpg", "--quality", "60"]);
        let plan = plan(Path::new("photo.jpg"), &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output.as_deref(), Some(Path::new("photo1.jpg")));
    }

    #[test]
    fn explicit_overwrite_keeps_the_input_name() {
        let args = args(&["primage", "photo.jpg", "--quality", "60", "--overwrite"]);
        let plan = plan(Path::new("photo.jpg"), &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output.as_deref(), Some(Path::new("photo.jpg")));
    }

    #[test]
    fn generated_name_collisions_are_numbered() {
        let args = args(&["primage", "photo.jpg", "--quality", "60"]);
        let mut taken = HashSet::from([PathBuf::from("photo1.jpg")]);
        let plan = plan(Path::new("photo.jpg"), &args, &mut taken).unwrap();
        assert_eq!(plan.output.as_deref(), Some(Path::new("photo2.jpg")));
    }

    #[test]
    fn encoded_jpeg_preview_contains_codec_output() {
        let source = RgbaImage::from_pixel(16, 16, Rgba([255, 0, 0, 0]));
        let bytes = codecs::encode(
            &source,
            Format::Jpeg,
            &codecs::EncodeOptions {
                quality: Some(100),
                lossless: false,
                png_level: 2,
                png_interlace: false,
                avif_speed: 6,
            },
        )
        .unwrap();
        let preview = decode_encoded_preview(&bytes, Format::Jpeg)
            .unwrap()
            .unwrap();
        let pixel = preview.get_pixel(8, 8).0;
        assert!(pixel[0] > 240 && pixel[1] > 240 && pixel[2] > 240);
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn encoded_avif_preview_is_explicitly_unavailable() {
        assert!(decode_encoded_preview(&[], Format::Avif).unwrap().is_none());
    }
}
