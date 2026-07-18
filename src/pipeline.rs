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

/// A fully resolved conversion job.
pub struct Plan {
    pub input: PathBuf,
    output: PathBuf,
    format: Format,
}

/// Result of processing one input file.
pub struct Report {
    pub input: PathBuf,
    pub output: PathBuf,
    pub input_size: u64,
    pub output_size: u64,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let change = size_change(self.input_size, self.output_size);
        write!(
            f,
            "{} → {}  {} → {}  ({change:+.1}%)",
            self.input.display(),
            self.output.display(),
            human_bytes(self.input_size),
            human_bytes(self.output_size),
        )
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
    let format = resolve_format(input, args)?;
    let output = resolve_output_path(input, args, format, taken)?;
    if output == input && !args.overwrite {
        bail!("output would overwrite the input file; pass --overwrite, --suffix or -o");
    }
    Ok(Plan {
        input: input.to_path_buf(),
        output,
        format,
    })
}

/// Execute a plan: decode, preprocess, encode, write.
pub fn run(plan: &Plan, args: &Cli) -> Result<Report> {
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

    let opts = codecs::EncodeOptions {
        quality: args.quality,
        lossless: args.lossless,
        png_level: args.png_level,
        png_interlace: args.png_interlace,
        avif_speed: args.avif_speed,
    };
    let bytes = codecs::encode(&img, plan.format, &opts)?;

    if let Some(parent) = plan.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&plan.output, &bytes)
        .with_context(|| format!("cannot write {}", plan.output.display()))?;

    Ok(Report {
        input: plan.input.clone(),
        output: plan.output.clone(),
        input_size,
        output_size: bytes.len() as u64,
    })
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
    let auto_name = |n: Option<u32>| {
        let counter = n.map(|n| format!("-{n}")).unwrap_or_default();
        format!("{stem}{}{counter}.{ext}", args.suffix)
    };

    let mut candidate = match &args.output {
        // Multiple inputs, or an existing directory: treat -o as a directory.
        Some(out) if args.inputs.len() > 1 || out.is_dir() => out.join(auto_name(None)),
        Some(out) => out.clone(),
        None => input.with_file_name(auto_name(None)),
    };
    let mut n = 1;
    while !taken.insert(candidate.clone()) {
        n += 1;
        candidate = match &args.output {
            Some(out) if args.inputs.len() > 1 || out.is_dir() => out.join(auto_name(Some(n))),
            Some(out) => out.with_file_name(auto_name(Some(n))),
            None => input.with_file_name(auto_name(Some(n))),
        };
    }
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
