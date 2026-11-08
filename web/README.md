# `pdf_web` — the viewer in the browser

`pdf_render` compiled to `wasm32-unknown-unknown`, with the SVG backend and no
`native` feature: no filesystem, no environment, no GPU stack. A page is
fetched by range, rendered to one self-contained SVG document, and put in the
DOM. Nothing is downloaded that the page does not read.

## The shape of it

```text
doc = pdf_doc_new(file_len)
pdf_fonts(doc, bundle)                  // all standard fonts, once, up front
while (needs = pdf_page_needs(doc, page)) is not empty:
    await fetch each range with a Range header, in parallel
    pdf_supply(doc, offset, bytes, len)  // one call per response
svg = pdf_page_svg(doc, page)            // UTF-8, free with pdf_buf_free
```

`pdf_page_needs(doc, 0xFFFF_FFFF)` is the same thing without a page: it opens
the document and says how many pages it has.

The ABI is plain `extern "C"` with **no imports at all**. `www/doc.js` checks
that — `WebAssembly.Module.imports(mod).length === 0` — and refuses a module
that wants anything. So the code that parses a stranger's PDF cannot reach the
network, the DOM or `eval`, and what the host has to audit is the export list:
`pdf_abi`, `pdf_doc_new/free`, `pdf_buf_alloc/free`, `pdf_supply`, `pdf_fonts`,
`pdf_page_needs`, `pdf_ranges_free`, `pdf_page_svg`, `pdf_num_pages`,
`pdf_held`, `pdf_error`.

## Building

`rustup target add wasm32-unknown-unknown` is all the toolchain needs.

```sh
cd web
cargo build --release                 # web/.cargo/config.toml picks the target
cargo run -p pdf_web --bin wasm_strip -- \
    "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/pdf_web.wasm" \
    www/pdf_web.wasm
```

The last step drops the `name` section: 900 kB of identifiers that only a
debugger reads, and free to remove because a custom section carries no
semantics. `wasm_report` says what is left, and what it weighs on the wire:

```sh
cargo run -p pdf_web --bin wasm_report -- www/pdf_web.wasm
```

| | bytes |
|---|---|
| `cargo build --release`, plain | 3,504,617 |
| … after removing `rand`/`getrandom`/`web-time` (see below) | 2,956,663 |
| … after `opt-level = "z"`, LTO, `panic = "abort"` | 3,014,623 (the name section grows under inlining) |
| … after dropping `name` — **this is the module** | **1,893,662** |
| gzip (`Content-Encoding: gzip`) | **651,861** (34.4 %) |
| brotli q11 (`Content-Encoding: br`) | **488,482** (25.8 %) |

Sections of the stripped module: `code` 1,446,109, `data` 438,964, everything
else under 8 kB. The `data` is mostly `pdf_encoding`'s glyph-name and codec
tables; a viewer that drops `/Differences` support could cut into it, which is
a product decision, not a build flag.

### Where the bytes are

Two tools, no toolchain needed beyond what built the module:

```sh
# add to [target.wasm32-unknown-unknown] rustflags: ["-C", "link-arg=--Map=<file>"]
cargo run -p pdf_web --bin wasm_map -- <map> [top]     # bytes by crate
cargo run -p pdf_web --bin wasm_strings -- <wasm> [filter]  # data-section strings
```

The map was taken while the brotli decoder was still linked (a 2,070,647 B
module), so its row is the one that can be subtracted: it attributes 1.38 MB of
the 2.06 MB, and linker stubs and the panic/format machinery stay unattributed.

| crate | bytes | of which code | why |
|---|---|---|---|
| `pdf` | 272,052 | 256,379 | the parser itself |
| `font` | 176,399 | 158,532 | Type 1/2, CFF, TrueType and the shapers |
| `brotli_decompressor` | 173,922 | 38,843 | **gone** — the bundle is decompressed by `Content-Encoding: br` |
| `pdf_encoding` | 151,343 | 16,419 | glyph names, codecs — nearly all data |
| `unicode_normalization` | 112,222 | 2,020 | Unicode tables, nearly all data |
| `core` | 80,131 | | `fmt` — panic messages and `Display` |
| `pdf_render` | 68,502 | 63,051 | 5 % — the renderer is not the problem |
| `miniz_oxide` + `fdeflate` + `libflate` | 68,333 | | one job, three inflate implementations |
| `image` + `zune_jpeg` + `png` + `weezl` | 92,072 | | image codecs |
| 30 more crates | ~150,000 | | |

The cuts taken and left: `brotli_decompressor` (174 kB, 135 kB of it brotli's
static dictionary) is gone — the server says `Content-Encoding: br` and the
network layer does the decoding, which took the module from 2,070,647 to
1,893,662 bytes and the response from 541,916 to 488,482. Collapsing the three
inflate crates would save ~68 kB; dropping GIF (`weezl`) and `stringprep`
~33 kB. None of it changes the fact that the module is one cacheable 488 kB
response.

The font bundle it needs is the one `fonts_blob` writes:
```sh
cargo run --release --features bundle-write --bin fonts_blob -- \
    standard_fonts web/www
# -> standard_fonts.br (547,303 bytes) plus the framed bundles, for reference
```

`standard_fonts.br` is the brotli stream on its own, to be served with
`Content-Encoding: br`: the browser inflates it and `pdf_fonts` is handed the
plain bundle, so the module carries no brotli decoder. nginx does this for a
file with that name — `brotli_static on;` — and serves the framed
`standard_fonts.gzip.bundle` just as well if only `gzip_static` is available
(`Content-Encoding: gzip`); `filter` is not needed for either.

`web/www/` is then a static site: `index.html`, `doc.js`, `pdf_web.wasm`,
`standard_fonts.br`. Serve it with any server that answers `Range` requests —
nginx does by default, and a hash-named wasm plus the font bundle can be cached
forever. The bundle is the whole set on purpose: one request, and which fonts a
document uses is not visible on the wire.

Cross-origin documents need CORS on the PDF's server: `Access-Control-Allow-
Headers: Range`, `Access-Control-Expose-Headers: Content-Length,
Content-Range`. Same-origin needs nothing.

## Two patches, so that the module imports nothing

The first build had 27 imports from `__wbindgen_placeholder__`: wasm-bindgen
glue for a clock and for entropy. Both came from crates that assume a browser
is something you call out to. Neither is needed to draw a page, and "imports
nothing" is the property worth keeping, so `web/.cargo/config.toml` patches
them — only for builds run from `web/`, the native build is untouched:

* **`web-time`** (pulled in by `globalcache`, which is `pdf_render`'s cache):
  `std::time::Instant` panics on `wasm32-unknown-unknown`, so the crate reads
  `Date.now()` and `performance.now()` through wasm-bindgen. `vendor/web-time`
  replaces it with a logical clock: one millisecond per timestamp, monotonic,
  no imports. On every other target it *is* `std::time`, so native behaviour
  is unchanged.
* **`font`**, branch `vello`: its Type 2 `random` operator (12 23) used
  `rand::thread_rng`, which on wasm seeds itself from
  `crypto.getRandomValues`. The patch (`src/type2.rs`, one function) uses a
  fixed-seed xorshift instead, which also makes rendering reproducible — the
  same document draws the same shapes on every run, which the SVG comparisons
  in the tests already assumed. **This one has no upstream home yet**: the
  commit sits on `s3bk/font`, branch `vello`, so the patch below is a git patch
  and nothing points outside this tree. When it lands upstream, delete the
  `[patch."https://github.com/pdf-rs/font"]` block and nothing else changes.

After both: `imports: 0`. `www/doc.js` still checks, because a check that
passes is worth more than a build that once passed.

A patched build rewrites `Cargo.lock` to record the patched sources, because a
patch is part of the resolution. Run `git restore Cargo.lock` after building
from `web/` — the committed lock must describe an unpatched tree.

## Running it without a browser

```sh
node web/test/smoke.mjs [file.pdf]
```

Instantiates `www/pdf_web.wasm` with an empty import object, fails if the
module wants anything, hands it the font bundle, then opens the PDF and renders
three pages by reading ranges out of the file in memory. It is the same code
path as the page, minus `fetch` and the DOM.

## Preloading the neighbours

With a range table a page turn is pure compute, so the viewer starts the next
and the previous page before anyone asks for them: `doc.prefetch(n)` fetches
what page `n` will read — bytes only, no rendering — and `index.html` calls it
for `page ± 1` as soon as a page is shown. Measured on `nasa.pdf` (521 pages):

| step | requests | traffic |
|---|---|---|
| open | 1 | 37,917 B |
| page 1 | 2 | 78,908 B |
| prefetch page 2 | 2 | 16,079 B — the part page 1 had not already brought |
| page 2 | **0** | **0 B** |

It is real traffic for a page nobody may visit — twice the bytes of a page per
page read — so `?preload=0` turns it off.

`node web/test/prefetch.mjs <file.pdf> [<file.pdf>.tbl]` checks it: `fetch` and
the two out-parameters are stubbed around `www/doc.js`, and the test fails if a
preloaded page still asks for a byte, if the same page is prefetched twice at
once, or if an edge page makes a request. (A document small enough to arrive in
the first request says so instead of passing.)

## How long a page takes

`node web/test/timing.mjs <file.pdf> [<file.pdf>.tbl] [all | page …]` runs the
viewer's own `doc.js` against the module with no browser and prints where the
milliseconds go. On `nasa.pdf`, over all 521 pages:

| | before | after |
|---|---|---|
| mean page | 83.6 ms | 7.8 ms |
| worst page | 524 ms | 32 ms |
| page turn in the browser | 102 ms | 31 ms (9 module + 7 DOM + 14 layout + 5 paint) |

93 % of the old cost was one function: `svg::prefix_ids` renames every `id`,
`href="#…"` and `url(#…)` of a layer so layers can share a document, and it
searched for all three markers again at every hit. On a page with 4,320 glyphs
`<use>`s and a marker that occurs twice, every hit after the last occurrence
scanned the rest of the page — quadratic, and the page's whole render time. It
now keeps each marker's next position and looks one up only when it walks past
it.

The rest of the rendering is where it should be: the SVG backend formats
coordinates itself (rounding to thousandths and writing digits, rather than
`format!("{:.3}")`, whose exact-decimal float formatting cost a fifth of a text
page), keeps glyph path data across pages keyed by the font's address, and
writes a paint's `fill`/`fill-opacity` once instead of once per glyph.

The viewer shows each page's own time in the readout (`page 20: 0 B in 0
request(s) · 156 KiB of 4.7 MiB read · 31 ms`) and puts the breakdown on
`document.body.dataset.timing`: `fetchModule`, `dom`, `layout`, `css`, `paint`.



## Swapping the font pack

`?fonts=<url>` puts another standard-font bundle in place of the shipped one.
Two packs then compare page for page through one viewer — and one
`standard_fonts.br` per pack in the asset store, since the name carries the
hash.

## What it costs, measured

`web/src/tests.rs` puts a simulated network in front of the same code path:
it answers exactly the ranges it is asked for, and the rendered page is
compared byte for byte with the page rendered from the whole file.

| document | page | round trips | bytes | share of file |
|---|---|---|---|---|
| `wasm/logo.pdf`, 3,280 B | 1 | 2 | all 3,280 | 100 % |
| `WG73-WriterGuide.pdf`, 25,268,940 B, 479 pages | open (page tree) | 2 | 34,816 | 0.14 % |
| | 1 (cover) | 7 | 3,116,283 | 12.3 % |
| | 3 (text) | 12 | 416,318 | 1.65 % |

Two things those numbers say, and one thing they hide:

* A text page is cheap: 416 kB of a 25 MB document, and the SVG it produces is
  238 kB with 104 paths.
* A page that is mostly one image pays for that image, which is the point.
* **Twelve round trips is too many.** Each pass stops at the first object it
  cannot read — a page tree node, a content stream, a font — so a page with
  ten fonts can cost ten passes even though each pass is one batch. Two fixes,
  both worth doing before this ships:
  1. a resource whose bytes are missing should be drawn as nothing and the
     pass should carry on, the way `pdf` already answers `null` for an object
     it cannot read; then one pass knows everything the page wants;
  2. the sender hands out the per-page table of ranges it computed when it
     published the file (`render/examples/net_sim.rs` measures both), which
     makes a page one round trip.

## Known gaps

* **The cover page renders empty.** `WG73-WriterGuide.pdf` page 1 draws
  nothing, and it does so from the whole file too — not a range problem. The
  2.86 MB it fetches is an image the SVG backend then drops.
* Inline images are counted, not drawn (`SvgBackend::draw_inline_image`).
* A raster layer is embedded as a PNG data URL, a third larger than the
  pixels. For a scanned page that is worth replacing with a second entry
  point that hands the host raw RGBA.
