//! The per-file pipeline: decode → preprocess → encode → write.
//!
//! Split into two phases so batches can be planned serially (output-name
//! resolution and deduplication) and then executed in parallel with rayon.

use std::collections::HashSet;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader};

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
    input_format: String,
    source_dimensions: (u32, u32),
    oriented_dimensions: (u32, u32),
    orientation: Orientation,
    transforms: Vec<String>,
    encoder: Option<String>,
    timings: StageTimings,
}

#[derive(Default)]
struct StageTimings {
    decode: Duration,
    transform: Duration,
    encode: Duration,
    write: Duration,
    total: Duration,
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

impl Report {
    pub fn format_with_verbosity(&self, verbosity: u8) -> String {
        if verbosity == 0 {
            return self.to_string();
        }

        let mut output = self.to_string();
        let _ = write!(
            output,
            "\n  input: {}, {}×{}",
            self.input_format, self.source_dimensions.0, self.source_dimensions.1
        );

        if self.orientation != Orientation::NoTransforms {
            let orientation = orientation_description(self.orientation);
            let _ = write!(
                output,
                "\n  orientation: {orientation} → {}×{}",
                self.oriented_dimensions.0, self.oriented_dimensions.1
            );
        }

        for transform in &self.transforms {
            let _ = write!(output, "\n  transform: {transform}");
        }
        if let Some(encoder) = &self.encoder {
            let _ = write!(output, "\n  encoder: {encoder}");
        }

        if verbosity >= 2 {
            let _ = write!(
                output,
                "\n  timing: decode {}, transform {}, encode {}, write {}, total {}",
                format_duration(self.timings.decode),
                format_duration(self.timings.transform),
                format_duration(self.timings.encode),
                format_duration(self.timings.write),
                format_duration(self.timings.total),
            );
        }
        output
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
    let total_started = Instant::now();
    let input_size = std::fs::metadata(&plan.input)
        .with_context(|| format!("cannot read {}", plan.input.display()))?
        .len();

    let decode_started = Instant::now();
    let reader = ImageReader::open(&plan.input)
        .with_context(|| format!("cannot open {}", plan.input.display()))?
        .with_guessed_format()?;
    let input_format = reader
        .format()
        .map(|format| format!("{format:?}").to_uppercase())
        .unwrap_or_else(|| "UNKNOWN".to_string());
    let mut decoder = reader
        .into_decoder()
        .with_context(|| format!("cannot create decoder for {}", plan.input.display()))?;
    let orientation = decoder
        .orientation()
        .with_context(|| format!("cannot read orientation from {}", plan.input.display()))?;
    let source_dimensions = decoder.dimensions();
    let mut decoded = DynamicImage::from_decoder(decoder)
        .with_context(|| format!("failed to decode {}", plan.input.display()))?;
    let decode_duration = decode_started.elapsed();

    let transform_started = Instant::now();
    decoded.apply_orientation(orientation);
    let oriented_dimensions = (decoded.width(), decoded.height());
    let mut img = decoded.to_rgba8();
    let mut transforms = Vec::new();

    // Preprocessors: rotate, then resize.
    if let Some(rotation) = args.rotate {
        img = transform::rotate(&img, rotation);
        transforms.push(format!(
            "rotate {} → {}×{}",
            rotation_description(rotation),
            img.width(),
            img.height()
        ));
    }
    match (args.resize, args.max_size) {
        (Some(geometry), _) => {
            let before = img.dimensions();
            img = transform::resize(
                &img,
                geometry.width,
                geometry.height,
                args.resize_filter.into(),
            );
            transforms.push(format!(
                "resize {}×{} → {}×{}, {:?}",
                before.0,
                before.1,
                img.width(),
                img.height(),
                args.resize_filter
            ));
        }
        (None, Some(max)) => {
            let before = img.dimensions();
            img = transform::fit_within(&img, max, args.resize_filter.into());
            transforms.push(format!(
                "max-size {max}: {}×{} → {}×{}, {:?}",
                before.0,
                before.1,
                img.width(),
                img.height(),
                args.resize_filter
            ));
        }
        (None, None) => {}
    }
    let transform_duration = transform_started.elapsed();

    let dimensions = (img.width(), img.height());
    let mut encoded_preview = None;
    let mut preview_warning = None;
    let mut encoder = None;
    let mut encode_duration = Duration::ZERO;
    let mut write_duration = Duration::ZERO;

    let output_size = match (plan.output.as_ref(), plan.format) {
        (Some(output), Some(format)) => {
            let opts = codecs::EncodeOptions {
                quality: args.quality,
                lossless: args.lossless,
                png_level: args.png_level.unwrap_or(2),
                png_interlace: args.png_interlace,
                avif_speed: args.avif_speed.unwrap_or(6),
            };
            encoder = Some(opts.describe(format));
            let encode_started = Instant::now();
            let bytes = codecs::encode(&img, format, &opts)?;
            encode_duration = encode_started.elapsed();

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
            let write_started = Instant::now();
            std::fs::write(output, &bytes)
                .with_context(|| format!("cannot write {}", output.display()))?;
            write_duration = write_started.elapsed();
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

    let timings = StageTimings {
        decode: decode_duration,
        transform: transform_duration,
        encode: encode_duration,
        write: write_duration,
        total: total_started.elapsed(),
    };

    Ok(Report {
        input: plan.input.clone(),
        output: plan.output.clone(),
        input_size,
        output_size,
        dimensions,
        image,
        preview_warning,
        input_format,
        source_dimensions,
        oriented_dimensions,
        orientation,
        transforms,
        encoder,
        timings,
    })
}

fn orientation_description(orientation: Orientation) -> &'static str {
    match orientation {
        Orientation::NoTransforms => "none",
        Orientation::Rotate90 => "rotate 90° clockwise",
        Orientation::Rotate180 => "rotate 180°",
        Orientation::Rotate270 => "rotate 270° clockwise",
        Orientation::FlipHorizontal => "flip horizontally",
        Orientation::FlipVertical => "flip vertically",
        Orientation::Rotate90FlipH => "rotate 90° clockwise and flip horizontally",
        Orientation::Rotate270FlipH => "rotate 270° clockwise and flip horizontally",
    }
}

fn rotation_description(rotation: crate::cli::Rotation) -> &'static str {
    match rotation {
        crate::cli::Rotation::R90 => "90° clockwise",
        crate::cli::Rotation::R180 => "180°",
        crate::cli::Rotation::R270 => "270° clockwise",
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
    }
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

/// Compute the output path for one input.
///
/// Implicit names use the preferred `stem + suffix + extension`, then append
/// `1`, `2`, ... to the complete stem when that path conflicts with an input,
/// an existing file, or another output planned in this batch. An explicit
/// output file is never renamed and requires `--overwrite` if it exists.
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
    let base = format!("{stem}{}", args.suffix);
    let auto_name = |number: Option<u32>| {
        let number = number.map(|number| number.to_string()).unwrap_or_default();
        format!("{base}{number}.{ext}")
    };

    let output_directory = args
        .output
        .as_ref()
        .filter(|out| args.inputs.len() > 1 || out.is_dir());

    // A single explicit output file represents an exact user choice. Do not
    // silently replace it or invent a different name.
    if let Some(output) = args.output.as_ref().filter(|_| output_directory.is_none()) {
        if taken.contains(output) {
            bail!("output path is used more than once: {}", output.display());
        }
        if output.exists() && !args.overwrite {
            bail!(
                "output already exists: {}; pass --overwrite to replace it",
                output.display()
            );
        }
        taken.insert(output.clone());
        return Ok(output.clone());
    }

    let path_for = |number: Option<u32>| match output_directory {
        Some(directory) => directory.join(auto_name(number)),
        None => input.with_file_name(auto_name(number)),
    };
    let conflicts = |candidate: &Path| {
        let other_input = args
            .inputs
            .iter()
            .any(|other| other != input && other == candidate);
        taken.contains(candidate)
            || other_input
            || (!args.overwrite && (candidate == input || candidate.exists()))
    };

    let mut candidate = path_for(None);
    let mut number = 1;
    while conflicts(&candidate) {
        candidate = path_for(Some(number));
        number += 1;
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
    use image::{ExtendedColorType, ImageEncoder, RgbImage, Rgba, RgbaImage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("primage-{label}-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct TempJpeg(PathBuf);

    impl TempJpeg {
        fn with_orientation(orientation: u8) -> Self {
            let image = RgbImage::from_fn(2, 3, |x, y| {
                image::Rgb([(x * 100) as u8, (y * 80) as u8, 120])
            });
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
                .write_image(image.as_raw(), 2, 3, ExtendedColorType::Rgb8)
                .unwrap();

            // JPEG APP1 marker containing a minimal little-endian TIFF IFD with
            // only the EXIF orientation tag (0x0112).
            let app1 = [
                0xff,
                0xe1,
                0x00,
                0x22, // marker length: two length bytes + 32-byte payload
                b'E',
                b'x',
                b'i',
                b'f',
                0,
                0,
                b'I',
                b'I',
                42,
                0,
                8,
                0,
                0,
                0, // first IFD offset
                1,
                0, // one directory entry
                0x12,
                0x01, // orientation tag
                3,
                0, // SHORT
                1,
                0,
                0,
                0, // one value
                orientation,
                0,
                0,
                0, // value plus padding
                0,
                0,
                0,
                0, // no next IFD
            ];
            jpeg.splice(2..2, app1);

            let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "primage-orientation-{}-{id}.jpg",
                std::process::id()
            ));
            std::fs::write(&path, jpeg).unwrap();
            Self(path)
        }
    }

    impl Drop for TempJpeg {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

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
    fn cross_format_conversion_uses_the_plain_stem_when_available() {
        let args = args(&["primage", "photo.png", "--format", "jpeg"]);
        let plan = plan(Path::new("photo.png"), &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output.as_deref(), Some(Path::new("photo.jpg")));
    }

    #[test]
    fn cross_format_file_collisions_append_a_number_to_the_stem() {
        let dir = TempDir::new("cross-format-collision");
        let input = dir.path("photo.png");
        std::fs::write(dir.path("photo.jpg"), []).unwrap();
        std::fs::write(dir.path("photo1.jpg"), []).unwrap();

        let args = args(&["primage", input.to_str().unwrap(), "--format", "jpeg"]);
        let plan = plan(&input, &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output, Some(dir.path("photo2.jpg")));
    }

    #[test]
    fn suffix_file_collisions_append_a_number_after_the_suffix() {
        let dir = TempDir::new("suffix-collision");
        let input = dir.path("photo.jpg");
        std::fs::write(dir.path("photo.min.jpg"), []).unwrap();

        let args = args(&["primage", input.to_str().unwrap(), "--suffix", ".min"]);
        let plan = plan(&input, &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output, Some(dir.path("photo.min1.jpg")));
    }

    #[test]
    fn duplicate_batch_names_are_numbered_consistently() {
        let dir = TempDir::new("batch-collision");
        let first = dir.path("first/photo.png");
        let second = dir.path("second/photo.png");
        let output = dir.path("output");
        let args = args(&[
            "primage",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
            "--format",
            "jpeg",
            "--output",
            output.to_str().unwrap(),
        ]);
        let mut taken = HashSet::new();

        let first_plan = plan(&first, &args, &mut taken).unwrap();
        let second_plan = plan(&second, &args, &mut taken).unwrap();

        assert_eq!(first_plan.output, Some(output.join("photo.jpg")));
        assert_eq!(second_plan.output, Some(output.join("photo1.jpg")));
    }

    #[test]
    fn explicit_existing_output_requires_overwrite() {
        let dir = TempDir::new("explicit-collision");
        let input = dir.path("photo.png");
        let output = dir.path("result.jpg");
        std::fs::write(&output, []).unwrap();
        let args = args(&[
            "primage",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ]);

        let error = match plan(&input, &args, &mut HashSet::new()) {
            Ok(_) => panic!("existing explicit output should require --overwrite"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("output already exists"));
        assert!(error.to_string().contains("--overwrite"));
    }

    #[test]
    fn overwrite_keeps_an_explicit_existing_output() {
        let dir = TempDir::new("explicit-overwrite");
        let input = dir.path("photo.png");
        let output = dir.path("result.jpg");
        std::fs::write(&output, []).unwrap();
        let args = args(&[
            "primage",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--overwrite",
        ]);

        let plan = plan(&input, &args, &mut HashSet::new()).unwrap();
        assert_eq!(plan.output, Some(output));
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

    #[test]
    fn exif_orientation_is_applied_before_user_transforms() {
        let input = TempJpeg::with_orientation(6);
        let input_str = input.0.to_str().unwrap();
        let args = args(&["primage", input_str, "--preview", "--rotate", "90", "-vv"]);
        let plan = plan(&input.0, &args, &mut HashSet::new()).unwrap();
        let report = run(&plan, &args, None).unwrap();

        assert_eq!(report.source_dimensions, (2, 3));
        assert_eq!(report.dimensions, (2, 3));
        assert_eq!(report.orientation, Orientation::Rotate90);

        let verbose = report.format_with_verbosity(args.verbose);
        assert!(verbose.contains("orientation: rotate 90° clockwise → 3×2"));
        assert!(verbose.contains("transform: rotate 90° clockwise → 2×3"));
        assert!(verbose.contains("timing: decode"));
    }

    #[test]
    fn verbose_output_omits_normal_orientation() {
        let input = TempJpeg::with_orientation(1);
        let input_str = input.0.to_str().unwrap();
        let args = args(&["primage", input_str, "--preview", "-v"]);
        let plan = plan(&input.0, &args, &mut HashSet::new()).unwrap();
        let report = run(&plan, &args, None).unwrap();

        assert!(!report
            .format_with_verbosity(args.verbose)
            .contains("orientation:"));
    }
}
