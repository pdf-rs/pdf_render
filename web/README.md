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

The target must be installed, and this container had to do it by hand because
`rustup` cannot run in its sandbox — if `rustup target add` works for you, it
does. Otherwise: take `rust-std-<version>-wasm32-unknown-unknown.tar.gz` from
<https://static.rust-lang.org/dist/>, unpack it, and copy its
`lib/rustlib/wasm32-unknown-unknown` next to the host target's.

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
| … after `opt-level = "z"`, LTO, `panic = "abort"` | 3,013,802 (the name section grows under inlining) |
| … after dropping `name` — **this is the module** | **2,069,826** |
| gzip (`Content-Encoding: gzip`) | **734,532** |

Sections of the stripped module: `code` 1,487,103, `data` 573,948, everything
else under 8 kB. The `data` is mostly `pdf_encoding`'s glyph-name and codec
tables; a viewer that drops `/Differences` support could cut into it, which is
a product decision, not a build flag.

The font bundle it needs is the one `fonts_blob` writes:

```sh
cargo run --release --features bundle-write --bin fonts_blob -- \
    standard_fonts ../fonts-blob      # -> standard_fonts.brotli.bundle
cp ../fonts-blob/standard_fonts.brotli.bundle web/www/
```

`web/www/` is then a static site: `index.html`, `doc.js`, `pdf_web.wasm`,
`standard_fonts.brotli.bundle`. Serve it with any server that answers `Range`
requests — nginx does by default, and a hash-named wasm plus the font bundle
can be cached forever.

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
  in the tests already assumed. **This one is not in the repository**: it is a
  local checkout at `../font-vello-b` until it is upstream, because a path
  patch to a directory outside the tree is not something to commit.

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
