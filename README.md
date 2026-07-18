# primage

Compress and convert images like [Squoosh](https://github.com/GoogleChromeLabs/squoosh) — as a fast CLI, in pure(ish) Rust.

Squoosh runs its codecs as WASM inside a browser (or Node). `primage` reimplements the same pipeline with the modern native-Rust codec ecosystem — no V8, no WASM runtime, a single small binary (~4 MB).

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

| feature   | default | effect                                                                 |
|-----------|---------|------------------------------------------------------------------------|
| `avif`    | ✓       | AVIF encoding via [ravif](https://crates.io/crates/ravif) + rav1e (pure Rust). Disable with `--no-default-features` for a much faster build. |
| `mozjpeg` | ✗       | Real [MozJPEG](https://crates.io/crates/mozjpeg) (the C library Squoosh uses) — progressive scans + trellis-class optimization, ~40% smaller JPEGs than the pure-Rust baseline encoder. Needs a C compiler. |

```sh
cargo build --release --features mozjpeg
```

## Usage

```console
primage [OPTIONS] <INPUT>...

-o, --output <PATH>        Output file, or directory when processing multiple inputs
-f, --format <FORMAT>      jpeg | png | webp | avif | qoi   (default: same as input)
-q, --quality <1-100>      Lossy quality (defaults: jpeg=75, avif=50)
    --resize <GEOMETRY>    WxH, Wx (auto height), xH (auto width)
    --rotate <90|180|270>  Rotate before encoding
    --resize-filter <F>    triangle | catrom | gaussian | lanczos3 | nearest
    --png-level <0-6>      OxiPNG effort (default: 2)
    --png-interlace        Adam7 interlacing
    --avif-speed <0-10>    AVIF speed (default: 6, like Squoosh)
-s, --suffix <SUFFIX>      Suffix for generated names, e.g. -s .min
    --overwrite            Allow overwriting the input file
```

Examples:

```sh
primage photo.jpg -q 60                        # recompress JPEG in place-style
primage photo.png -f webp                      # PNG → lossless WebP
primage *.png -f avif -o out/                  # batch convert into out/
primage big.tiff --resize 1600x -f jpg -q 80   # TIFF → resized JPEG
primage scan.png --rotate 90 -f png --png-level 6
```

Input decoding: JPEG, PNG, WebP, GIF, TIFF, BMP, ICO, TGA, PNM, QOI (8-bit RGBA pipeline, like Squoosh's `ImageData`).

## Squoosh → primage mapping

| Squoosh codec | primage backend | notes |
|---|---|---|
| MozJPEG | `image` crate baseline JPEG / `mozjpeg` crate (opt-in) | same default quality 75; alpha flattened to white |
| OxiPNG | [`oxipng`](https://crates.io/crates/oxipng) crate | literally the same code Squoosh compiles to WASM |
| WebP | [`image-webp`](https://crates.io/crates/image-webp) | **lossless only** — see below |
| AVIF | [`ravif`](https://crates.io/crates/ravif) (rav1e) | modern pure-Rust replacement for Squoosh's libaom WASM; same defaults (q50, speed 6) |
| QOI | `image` crate | ✓ |
| JPEG XL | ✗ | no mature pure-Rust encoder yet ([`jxl-oxide`](https://crates.io/crates/jxl-oxide) is decode-only) |
| WebP2 | ✗ | format discontinued |

| Squoosh processor | primage |
|---|---|
| Resize (triangle / catrom / lanczos3 / …) | `--resize` + `--resize-filter` (mitchell ≈ gaussian) |
| Rotate | `--rotate 90/180/270` |
| Quantize (imagequant) | not yet |

## How pure-Rust is it?

- Default build: **100% safe Rust codecs** (image-rs ecosystem, zopfli, rav1e), with one exception — `oxipng` internally bundles [libdeflate](https://github.com/ebiggers/libdeflate) (tiny, safe C) for fast deflate trials; the heavy compression is pure-Rust zopfli.
- `--features mozjpeg` adds the real MozJPEG C library, exactly the codec Squoosh uses. JPEG is the one format where pure Rust still can't compete: mozjpeg's progressive scans + optimized Huffman coding produced **~40% smaller** files in testing.
- Lossy WebP is the other gap: no pure-Rust VP8 encoder exists yet ([image-webp roadmap](https://github.com/image-rs/image-webp)). WebP output is therefore lossless for now.

## Roadmap

- [ ] Lossy WebP when `image-webp` gains an encoder
- [ ] JPEG XL decoding via `jxl-oxide`
- [ ] AVIF decoding when a pure-Rust decoder (e.g. rav1d) matures
- [ ] Palette quantization via [`imagequant`](https://crates.io/crates/imagequant)
- [ ] `fast_image_resize` SIMD resizing

## License

MIT OR Apache-2.0
