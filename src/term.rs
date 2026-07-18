//! Terminal image display via the Kitty graphics protocol
//! (supported by Ghostty, kitty, WezTerm, Konsole, …).

use std::io::{IsTerminal, Write};

use anyhow::{bail, Result};
use base64::Engine as _;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

/// Payload chunk size required by the protocol.
const CHUNK_SIZE: usize = 4096;

/// Terminal dimensions used to prepare and place previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayConfig {
    pixel_width: u32,
}

impl DisplayConfig {
    pub fn detect() -> Result<Self> {
        Self::from_pixel_size(terminal_pixel_size())
    }

    fn from_pixel_size(size: Option<(u32, u32)>) -> Result<Self> {
        let Some((pixel_width, _)) = size else {
            bail!(
                "Terminal does not support reporting screen sizes in pixels, \
                 use a terminal such as kitty, WezTerm, Konsole, etc. that does."
            );
        };
        Ok(Self { pixel_width })
    }

    pub fn pixel_width(self) -> u32 {
        self.pixel_width
    }
}

#[cfg(any(unix, test))]
fn valid_pixel_size(width: u16, height: u16) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        None
    } else {
        Some((u32::from(width), u32::from(height)))
    }
}

#[cfg(unix)]
fn terminal_pixel_size() -> Option<(u32, u32)> {
    let size = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    valid_pixel_size(size.ws_xpixel, size.ws_ypixel)
}

#[cfg(not(unix))]
fn terminal_pixel_size() -> Option<(u32, u32)> {
    None
}

/// Heuristic support detection via the environment — terminals known to
/// implement the Kitty graphics protocol. Never claim support when stdout
/// isn't a TTY (would dump escape codes into pipes/files).
pub fn supports_kitty() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    term.contains("kitty")
        || term.contains("ghostty")
        || matches!(term_program.as_str(), "ghostty" | "WezTerm" | "kitty")
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || std::env::var_os("KONSOLE_VERSION").is_some()
}

/// Render an image in the terminal at its prepared pixel size.
pub fn display(img: &RgbaImage) -> Result<()> {
    // Transmit as PNG (f=100): compact and universally supported.
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgba8,
    )?;

    let mut stdout = std::io::stdout().lock();
    write_kitty(&mut stdout, &png)?;
    stdout.flush()?;
    Ok(())
}

/// Write the escape sequence using the same cursor behavior as `kitten icat`.
///
/// The terminal moves the cursor past the placement when `C` is left at its
/// default value. Only one trailing newline is needed to put subsequent output
/// at the start of the next line; emitting one newline per image row would
/// scroll the image straight out of view in spec-compliant terminals.
fn write_kitty<W: Write>(w: &mut W, png: &[u8]) -> std::io::Result<()> {
    w.write_all(b"\r")?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks = b64.as_bytes().chunks(CHUNK_SIZE);
    let count = chunks.len();
    for (i, chunk) in chunks.enumerate() {
        let more = u8::from(i + 1 < count);
        if i == 0 {
            // No c/r placement size: the image was already prepared at the
            // terminal's native pixel width and must not be resampled.
            write!(w, "\x1b_Ga=T,f=100,q=2,m={more};")?;
        } else {
            write!(w, "\x1b_Gm={more};")?;
        }
        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;
    }
    w.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_size_must_include_both_dimensions() {
        assert_eq!(valid_pixel_size(1200, 800), Some((1200, 800)));
        assert_eq!(valid_pixel_size(0, 800), None);
        assert_eq!(valid_pixel_size(1200, 0), None);
    }

    #[test]
    fn missing_pixel_size_has_icat_style_error() {
        let error = DisplayConfig::from_pixel_size(None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Terminal does not support reporting screen sizes in pixels, \
             use a terminal such as kitty, WezTerm, Konsole, etc. that does."
        );
    }

    #[test]
    fn small_payload_is_single_chunk() {
        let mut out = Vec::new();
        write_kitty(&mut out, b"abc").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "\r\x1b_Ga=T,f=100,q=2,m=0;YWJj\x1b\\\n");
        assert!(!s.contains(",c="));
        assert!(!s.contains(",r="));
    }

    #[test]
    fn large_payload_is_chunked() {
        let png = vec![0u8; 10_000]; // ~13.3k base64 chars → 4 chunks
        let mut out = Vec::new();
        write_kitty(&mut out, &png).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\r\x1b_Ga=T,f=100,q=2,m=1;"));
        let chunks: Vec<_> = s.split("\x1b\\").collect();
        assert_eq!(chunks.len() - 1, 4); // 4 escape sequences
        assert!(chunks[3].ends_with("m=0;AAA") || chunks[3].contains("m=0;"));
        assert!(!chunks[1].contains("a=T")); // only the first chunk carries control data
        assert_eq!(s.chars().filter(|&c| c == '\n').count(), 1);
    }
}
