//! Render a page from a document that has only partly arrived.
//!
//! usage: cargo run --features svg --example range_render -- <file.pdf> <page> <out.svg> [head KiB] [tail KiB]
//!
//! The document is on disk, but the cache starts with only two windows of it:
//! the head (what a first response would carry) and the tail (the trailer and
//! `startxref`). Everything else is fetched on demand, in rounds: each round
//! renders once from the bytes it has, collects the ranges it was missing as
//! one list, fetches that list, and starts over. The pass is thrown away every
//! round, so a page that rendered from partial bytes is never shown — the
//! result returned is from a round that missed nothing.
//!
//! What makes the rounds few is that a missing object does not stop the pass:
//! `pdf` answers `null` for an object whose bytes are not here (7.3.9), so the
//! walk keeps going and finds the other ranges it needs in the same round.

use std::ops::Range;

use pdf::backend::{Backend, IndexRange};
use pdf::error::Result as PdfResult;
use pdf::file::{File, NoCache, NoLog};
use pdf::object::ParseOptions;
use pdf_render::rangecache::{render_with_ranges, LazyReader, RangeCache};
use pdf_render::svg_backend::{OutlineBuilder, SvgBackend};
use pdf_render::{render_page, Cache};
use pathfinder_geometry::transform2d::Transform2F;

/// A `pdf::Backend` over a borrowed reader.
///
/// `Backend` can not be implemented for `&B` in the crate itself (it collides
/// with the blanket impl for `Deref<Target=[u8]>`), so the borrow gets a type
/// of its own.
struct Borrowed<'a, 'b>(&'a LazyReader<'b>);

impl Backend for Borrowed<'_, '_> {
    fn read<T: IndexRange>(&self, range: T) -> PdfResult<&[u8]> {
        self.0.read(range)
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn is_incomplete(&self, range: Range<usize>) -> bool {
        self.0.is_incomplete(range)
    }
}

fn main() {
    // `pdf` warns when a bounded read had to be retried open-ended — exactly
    // what makes a round cost more than it should — so keep that visible.
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "warn");
    }
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: range_render <file.pdf> <page> <out.svg> [head KiB] [tail KiB]");
        return;
    }
    let path = &args[1];
    let page_nr: u32 = args[2].parse().expect("page must be a number");
    let out = &args[3];
    let head_kib: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
    let tail_kib: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(4);

    if std::env::var_os("STANDARD_FONTS").is_none() {
        let local = if std::path::Path::new("fonts").is_dir() { "fonts" } else { "standard_fonts" };
        if std::path::Path::new(local).is_dir() {
            std::env::set_var("STANDARD_FONTS", local);
        }
    }

    // The "network": the whole document.
    let server = std::fs::read(path).expect("cannot read the file");
    let len = server.len();

    let mut cache = RangeCache::new(len);
    let head = len.min(head_kib * 1024);
    cache.insert(0, server[..head].to_vec());
    let tail = len.saturating_sub(head).min(tail_kib * 1024);
    if tail > 0 {
        cache.insert(len - tail, server[len - tail..].to_vec());
    }
    println!(
        "{len} bytes, starting with {} bytes of head and {} of tail",
        head, tail
    );

    // One entry per round: the ranges fetched, and how many bytes they were.
    let mut rounds: Vec<(Vec<Range<usize>>, usize)> = Vec::new();

    let rendered = render_with_ranges(
        &mut cache,
        12,
        |reader| {
            let file = File::load_data(
                Borrowed(reader),
                b"",
                ParseOptions::strict(),
                NoCache,
                NoCache,
                NoLog,
            )?;
            let page = file.get_page(page_nr - 1)?;
            let resolver = file.resolver();
            let mut glyphs = Cache::new(OutlineBuilder::default());
            let mut backend = SvgBackend::new(&mut glyphs);
            render_page(&mut backend, &resolver, &page, Transform2F::default())?;
            let page = backend.finish();
            page.single_svg()
                .map(str::to_owned)
                .ok_or_else(|| pdf::PdfError::Other {
                    msg: "this page has image layers; it is not a single svg".into(),
                })
        },
        |cache, missing| {
            // Every miss costs a round trip, so make each one worth more than
            // the object that caused it: objects that are needed together are
            // usually written near each other, so take a little either side.
            // This is the client's own policy — the range table a sender can
            // hand out is what makes the first round enough.
            const SLACK: usize = 64 * 1024;
            let widened: Vec<Range<usize>> = missing
                .iter()
                .map(|r| r.start.saturating_sub(SLACK) .. (r.end + SLACK).min(len))
                .collect();
            let mut bytes = 0;
            for r in &widened {
                for part in cache.missing(std::slice::from_ref(r)) {
                    cache.insert(part.start, server[part.clone()].to_vec());
                    bytes += part.len();
                }
            }
            println!(
                "round {}: {} range(s) fetched, {} bytes",
                rounds.len() + 1,
                widened.len(),
                bytes
            );
            for r in &widened {
                println!("    {:?}", r);
            }
            rounds.push((widened, bytes));
            Ok(())
        },
    );

    let svg = match rendered {
        Ok(svg) => svg,
        Err(e) => {
            eprintln!("failed after {} round(s): {e}", rounds.len());
            std::process::exit(1);
        }
    };

    let fetched: usize = rounds.iter().map(|(_, bytes)| *bytes).sum();
    let ranges: usize = rounds.iter().map(|(r, _)| r.len()).sum();
    println!(
        "page {page_nr}: {} bytes of svg, {} glyph(s) worth of outline cache",
        svg.len(),
        glyphs_len(&svg),
    );
    println!(
        "{} round(s), {} range(s), {} bytes fetched of {len} ({:.1}%), {} bytes in the cache",
        rounds.len(),
        ranges,
        fetched,
        100.0 * fetched as f64 / len as f64,
        cache.held(),
    );
    std::fs::write(out, &svg).expect("cannot write svg");
    println!("-> {out}");
}

/// The `<symbol>` count of the file, i.e. distinct glyph outlines reused.
fn glyphs_len(svg: &str) -> usize {
    svg.matches("<symbol").count()
}
