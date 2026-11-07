# Standard 14 fonts we may ship

`STANDARD_FONTS=standard_fonts` (or `cargo build --features embed` with
`STANDARD_FONTS=standard_fonts` at build time) makes the renderer use this
directory instead of Adobe's substitutes. Nothing here is under a licence that
restricts redistribution, and nothing is derived from an Adobe font file.

| PDF base font | file | licence |
|---|---|---|
| Helvetica, Arial (+ bold/italic/bolditalic) | `texgyreheros-*.otf` | GUST Font License (LPPL 1.3c + rename request) |
| Times, Times New Roman (+ variants) | `texgyretermes-*.otf` | GUST Font License |
| Courier, Courier New (+ variants) | `texgyrecursor-*.otf` | GUST Font License |
| Symbol | `FoxitSymbol.pfb` | BSD 3-clause (PDFium / Foxit) |
| ZapfDingbats | `FoxitDingbats.pfb` | BSD 3-clause (PDFium / Foxit) |

`GUST-FONT-LICENSE.txt` and `LICENSE_FOXIT` are the licence texts; both are
reproduced verbatim. The GUST licence asks (does not require) that modified
fonts be renamed — these files are unmodified upstream releases, so keep them
that way or rename the files when you change outlines.

Sources, unmodified:

* TeX Gyre 2.004 OTF, `https://mirrors.ctan.org/fonts/tex-gyre/opentype/`
* PDFium's Foxit fonts, as shipped by pdf.js in
  `mozilla/pdf.js/external/standard_fonts/` (`LICENSE_FOXIT`, "Copyright 2014
  PDFium Authors", "Original code copyright 2014 Foxit Software Inc.")

## The bundle

A viewer in a browser cannot read a directory, and it must not fetch one font at
a time: which fonts a document uses would then be visible on the wire. So this
whole directory goes out as one resource, built by

    cargo run --features bundle-write --bin fonts_blob -- standard_fonts <out-dir>

and read by `MemDir::parse_bundle` → `StandardCache::with_dir`. It contains
`fonts.json` and the files it names — not the licence texts, not the `FoxitSerif`
/ `FoxitFixed` files nothing references.

`standard_fonts/` is 1,507,379 bytes. Compressed:

| codec | bytes | of raw | decoder in wasm |
|---|---|---|---|
| brotli q11, lgwin 22 | **547,313** | 36.3 % | `brotli-decompressor`, pure Rust |
| gzip -9 | 838,943 | 55.7 % | free: `DecompressionStream("gzip")` in the host |
| deflate -9 | 838,925 | 55.7 % | free, as above |
| none | 1,507,883 | 100 % | — |

The whole file is compressed as one stream, which is the point: brotli's 4 MiB
window covers every font at once, so the outlines that one weight shares with
the next are coded once. Per file, brotli reaches 700,389 bytes and gzip 834,200
— with gzip's 32 KiB window, smaller than one font, a bundle buys nothing at all.
Brotli's font mode (the WOFF2 one) is 547,946: no better than generic here,
because these are CFF fonts, and the mode's gain comes from glyph transforms
this blob does not apply.

Squeezing further means subsetting, and a standard-14 substitute cannot be
subset: a PDF may name any glyph in `/Differences`, and a missing glyph is a
blank box. The file is what it has to be; the compression is as far as it goes.

## Why these and not PDFium's other fonts

PDFium ships Foxit outlines for serif, monospaced, symbol and dingbats, but no
sans face — pdf.js fills that gap with Liberation Sans. TeX Gyre Heros, Termes
and Cursor are derived from the same URW designs that Ghostscript, poppler and
mupdf substitute for the standard 14, so a non-embedded `Helvetica` renders here
the way the reference engines render it. `FoxitSerif*.pfb` and `FoxitFixed*.pfb`
are kept so the whole set can be swapped to PDFium's own designs by editing
`fonts.json`; they are not referenced by default.

## Measured effect

Same corpus, same engine (poppler's `pdftoppm`, 300 dpi), only `STANDARD_FONTS`
differs — `fonts/` (Adobe Myriad Pro / Minion Pro substitutes) versus this
directory. Numbers are mean difference / differing pixels / structure error, all
lower is better:

| document | `fonts/` (Adobe) | `standard_fonts/` (this set) |
|---|---|---|
| pdf-sample (no embedded fonts) | 1.28 % / 1.90 % / 0.05 % | **0.75 % / 1.34 % / 0.02 %** |
| example (Helvetica text) | 0.80 % / 1.08 % / 0.19 % | **0.23 % / 0.48 % / 0.04 %** |
| xelatex (embedded fonts) | 0.20 % / 0.40 % / 0.01 % | 0.20 % / 0.40 % / 0.01 % |
| logo (embedded fonts) | 0.23 % / 0.29 % / 0.21 % | 0.23 % / 0.29 % / 0.21 % |

Pages with embedded fonts are identical, as they must be — only the pages that
fall back to a standard 14 change, and they move towards the reference.
