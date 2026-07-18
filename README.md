# primage

A fast CLI for compressing and converting images — JPEG, PNG, WebP, AVIF and QOI. Inspired by [Squoosh](https://github.com/GoogleChromeLabs/squoosh).

It uses best-in-class codec libraries natively — MozJPEG, libwebp, OxiPNG — plus modern pure-Rust alternatives (ravif/rav1e for AVIF), all statically linked into a single ~5 MB binary with no runtime dependencies.

```console
$ primage photo.png -f avif
photo.png → photo.avif  3.3 MB → 44.0 KB  (-98.7%)
```

## Install

```sh
cargo build --release          # binary in target/release/primage
# or
cargo install --path .
```

### Cargo features

| feature   | default | codec backend |
|-----------|---------|---------------|
| `mozjpeg` | ✓ | Real MozJPEG, statically linked. Off → pure-Rust baseline JPEG. |
| `libwebp` | ✓ | Lossy WebP via libwebp, statically linked. Off → pure-Rust lossless WebP only. |
| `avif`    | ✓ | AVIF via [ravif](https://crates.io/crates/ravif) + rav1e, 100% pure Rust. Disable for a much faster build. |

```sh
cargo build --release --no-default-features --features avif   # C-free build
```

## Usage

```console
primage [OPTIONS] <INPUT>...

-o, --output <PATH>        Output file, or directory when processing multiple inputs
-f, --format <FORMAT>      jpeg | png | webp | avif | qoi   (default: same as input)
-q, --quality <1-100>      Lossy quality (defaults: jpeg=75, webp=75, avif=50)
    --lossless             Lossless WebP compression
    --resize <GEOMETRY>    WxH, Wx (auto height), xH (auto width)
    --max-size <PX>        Shrink so the longest side is at most PX (keeps aspect)
    --rotate <90|180|270>  Rotate before encoding
    --resize-filter <F>    triangle | catrom | gaussian | lanczos3 | nearest
    --png-level <0-6>      OxiPNG effort (default: 2)
    --png-interlace        Adam7 interlacing
    --avif-speed <0-10>    AVIF encoder speed (default: 6)
-s, --suffix <SUFFIX>      Suffix for generated names, e.g. -s .min
    --overwrite            Allow overwriting the input file
    --preview              Display the image in the terminal (Kitty protocol)
```

Examples:

```sh
primage photo.jpg -q 60                        # recompress a JPEG
primage photo.png -f webp                      # PNG → lossy WebP (78 KB from 3.3 MB)
primage *.png -f avif -o out/                  # batch convert, parallel across cores
primage big.tiff --max-size 1600 -f jpg -q 80  # TIFF → resized JPEG
primage scan.png --rotate 90 -f png --png-level 6
primage icon.png -f webp --lossless            # lossless WebP

primage --preview photo.png                    # just view, writes nothing
primage photo.png -f avif --preview            # convert, then preview the result
```

## Terminal previews

`--preview` renders images inline via the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) — supported by **Ghostty**, kitty, WezTerm and Konsole. Handy for checking results without leaving the terminal:

- `primage --preview photo.png` — preview-only mode: decodes, applies any transforms and displays, without writing a file
- `--preview` combined with `-f`/`-o` converts first, then shows the processed image

Preview support is auto-detected from the environment and disabled when stdout isn't a TTY (pipes stay clean).
primage requires the terminal to report its pixel dimensions so it can prepare
a native-resolution preview without an additional terminal-side resize.

Input decoding: JPEG, PNG, WebP, GIF, TIFF, BMP, ICO, TGA, PNM, QOI (8-bit RGBA pipeline).

## Codecs

| Format | Backend | Defaults |
|---|---|---|
| JPEG | [`mozjpeg`](https://crates.io/crates/mozjpeg) | q75, progressive, optimized Huffman coding, auto 4:2:0/4:4:4 chroma subsampling |
| PNG | [`oxipng`](https://crates.io/crates/oxipng) | effort level 2, optional Adam7 interlacing |
| WebP | libwebp via the [`webp`](https://crates.io/crates/webp) crate | q75 lossy (method 4), or `--lossless` |
| AVIF | [`ravif`](https://crates.io/crates/ravif) + rav1e (pure Rust) | q50, speed 6 |
| QOI | [`image`](https://crates.io/crates/image) crate | — |

Not supported yet: JPEG XL (no mature pure-Rust encoder; [`jxl-oxide`](https://crates.io/crates/jxl-oxide) is decode-only), palette quantization.

## Portability

The release binary is self-contained: mozjpeg, libwebp and oxipng's bundled libdeflate are compiled in statically (`otool -L` shows only system libraries), so the binary can be copied and run anywhere on the same OS/arch. JPEG encoding flattens alpha onto white; everything runs on an 8-bit RGBA pipeline.

## Roadmap

- [ ] JPEG XL decoding via `jxl-oxide`
- [ ] AVIF decoding when a pure-Rust decoder (e.g. rav1d) matures
- [ ] Palette quantization via [`imagequant`](https://crates.io/crates/imagequant)
- [ ] `fast_image_resize` SIMD resizing

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
