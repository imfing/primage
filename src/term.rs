//! Terminal image display via the Kitty graphics protocol
//! (supported by Ghostty, kitty, WezTerm, Konsole, …).

use std::io::{IsTerminal, Write};

use anyhow::Result;
use base64::Engine as _;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

/// Payload chunk size required by the protocol.
const CHUNK_SIZE: usize = 4096;

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

/// Render an image in the terminal, scaled to fit.
pub fn display(img: &RgbaImage) -> Result<()> {
    let (term_cols, term_rows) = terminal_size::terminal_size()
        .map(|(w, h)| (u32::from(w.0), u32::from(h.0)))
        .unwrap_or((80, 24));
    let (cols, rows) = display_cells(img.width(), img.height(), term_cols, term_rows);

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
    write_kitty(&mut stdout, &png, cols, rows)?;
    stdout.flush()?;
    Ok(())
}

/// Write the escape sequence: chunked base64, then move the cursor below
/// the image so subsequent output doesn't overwrite it.
fn write_kitty<W: Write>(w: &mut W, png: &[u8], cols: u32, rows: u32) -> std::io::Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks = b64.as_bytes().chunks(CHUNK_SIZE);
    let count = chunks.len();
    for (i, chunk) in chunks.enumerate() {
        let more = u8::from(i + 1 < count);
        if i == 0 {
            // a=T: transmit & display; q=2: suppress terminal responses;
            // c,r: display area in cells (terminal scales to fit).
            write!(w, "\x1b_Ga=T,f=100,q=2,c={cols},r={rows},m={more};")?;
        } else {
            write!(w, "\x1b_Gm={more};")?;
        }
        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;
    }
    write!(w, "{}", "\n".repeat(rows as usize))
}

/// Fit (w, h) into (max_cols, max_rows) terminal cells, preserving aspect
/// ratio. A character cell is roughly twice as tall as it is wide.
fn display_cells(w: u32, h: u32, max_cols: u32, max_rows: u32) -> (u32, u32) {
    let cols = max_cols.max(1);
    let rows = (f64::from(cols) * f64::from(h) / (2.0 * f64::from(w))).ceil() as u32;
    if rows <= max_rows.max(1) {
        (cols, rows.max(1))
    } else {
        let rows = max_rows.max(1);
        let cols = (2.0 * f64::from(rows) * f64::from(w) / f64::from(h))
            .ceil()
            .min(f64::from(max_cols.max(1))) as u32;
        (cols.max(1), rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_wide_image() {
        // 2:1 image in 80x24 terminal: full width, 20 rows.
        assert_eq!(display_cells(2000, 1000, 80, 24), (80, 20));
    }

    #[test]
    fn fit_tall_image() {
        // 1:2 image in 80x24 terminal: capped to 24 rows, 24 cols.
        assert_eq!(display_cells(1000, 2000, 80, 24), (24, 24));
    }

    #[test]
    fn never_zero() {
        let (c, r) = display_cells(1, 10000, 80, 24);
        assert!(c >= 1 && r >= 1);
    }

    #[test]
    fn small_payload_is_single_chunk() {
        let mut out = Vec::new();
        write_kitty(&mut out, b"abc", 80, 20).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=100,q=2,c=80,r=20,m=0;YWJj\x1b\\"));
        assert!(s.ends_with(&"\n".repeat(20)));
    }

    #[test]
    fn large_payload_is_chunked() {
        let png = vec![0u8; 10_000]; // ~13.3k base64 chars → 4 chunks
        let mut out = Vec::new();
        write_kitty(&mut out, &png, 10, 5).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=100,q=2,c=10,r=5,m=1;"));
        let chunks: Vec<_> = s.split("\x1b\\").collect();
        assert_eq!(chunks.len() - 1, 4); // 4 escape sequences
        assert!(chunks[3].ends_with("m=0;AAA") || chunks[3].contains("m=0;"));
        assert!(!chunks[1].contains("a=T")); // only the first chunk carries control data
    }
}
