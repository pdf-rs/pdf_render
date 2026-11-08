//! The sender's side of a per-page range table.
//!
//! usage: cargo run --bin page_table -- <file.pdf> [out.tbl]
//!
//! Whoever holds the whole document — the machine that builds a share, an
//! origin server, a CDN — runs this once and ships the result next to the PDF.
//! A client that has it needs one round trip per page: it asks for exactly the
//! ranges the page reads, instead of rendering, failing, collecting the misses
//! and asking again.
//!
//! The table is produced by really rendering every page against a backend that
//! writes down what it reads, so it can not be short of anything the renderer
//! touches — the xref, the page tree, the fonts, the content stream and every
//! object the content reaches. Every page gets a **fresh** document object, the
//! way a client without a cache sees it: a table that leaned on an object it
//! had parsed for an earlier page would leave that object out, and the client
//! would pay a discovery round for it.
//!
//! Line 0 of the table is the document itself (what it costs to count the
//! pages), so the client's very first request is complete too.
//!
//! It is a text file (`pdfranges 1`), one line per page, and it compresses to
//! almost nothing, so serve it with the `.br` or gzip the server already does.
//!
//! Rendering needs the standard fonts: point `STANDARD_FONTS` at them, or run
//! this from a directory that has `fonts/` or `standard_fonts/`, exactly like
//! the examples do.

use std::ops::Range;
use std::time::Instant;

use pdf_render::pdf::file::{File, NoCache, NoLog};
use pdf_render::pdf::object::ParseOptions;
use pdf_render::rangecache::{format_table, Borrowed, Recorder};
use pdf_render::svg_backend::{OutlineBuilder, SvgBackend};
use pdf_render::{render_page, Cache, Transform2F};

/// How close two reads have to be to share a range. Every range costs a byte
/// range header and a slice of the request, so a few kilobytes of slack are
/// cheaper than a second header.
const SLACK: usize = 4 * 1024;

/// A document over a recorder, with nothing cached: the state a client that
/// has just arrived is in.
type Fresh<'a> = File<Borrowed<'a, Recorder<'a>>, NoCache, NoCache, NoLog>;

fn usage() {
    eprintln!("usage: page_table <file.pdf> [out.tbl]  (out.tbl defaults to <file.pdf>.tbl)");
}

/// Open `data` from nothing, run `f`, and report what it read.
fn recorded<R>(data: &[u8], f: impl FnOnce(&Fresh<'_>) -> R) -> Option<(R, Vec<Range<usize>>)> {
    let rec = Recorder::new(data);
    let file = match File::load_data(Borrowed(&rec), b"", ParseOptions::strict(), NoCache, NoCache, NoLog) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("cannot open: {e}");
            return None;
        }
    };
    let out = f(&file);
    Some((out, rec.take(SLACK)))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        usage();
        return;
    };
    let out = args.get(2).cloned().unwrap_or_else(|| format!("{path}.tbl"));

    if std::env::var_os("STANDARD_FONTS").is_none() {
        let local = if std::path::Path::new("fonts").is_dir() {
            "fonts"
        } else {
            "standard_fonts"
        };
        if std::path::Path::new(local).is_dir() {
            std::env::set_var("STANDARD_FONTS", local);
        }
    }

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };

    let started = Instant::now();

    // Page 0: the document, as a client sees it before it knows how many pages
    // there are.
    let Some((page_count, document)) = recorded(&data, |file| file.num_pages()) else {
        eprintln!("{path}: cannot open the document");
        std::process::exit(1);
    };

    let mut pages = Vec::with_capacity(page_count as usize + 1);
    pages.push((0u32, document));
    let mut failed = 0usize;
    for i in 0..page_count {
        // A fresh document per page: what a client has for a page it opens
        // cold, and the only way the table can be complete for that page.
        let rendered = recorded(&data, |file| {
            let page = match file.get_page(i) {
                Ok(page) => page,
                Err(e) => {
                    eprintln!("page {}: {e}", i + 1);
                    return false;
                }
            };
            let resolver = file.resolver();
            let mut glyphs = Cache::new(OutlineBuilder::default());
            let mut backend = SvgBackend::new(&mut glyphs);
            let out = render_page(&mut backend, &resolver, &page, Transform2F::default());
            drop(backend);
            match out {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("page {}: {e}", i + 1);
                    false
                }
            }
        });
        let Some((ok, ranges)) = rendered else {
            failed += 1;
            continue;
        };
        if !ok || ranges.is_empty() {
            failed += 1;
            continue;
        }
        pages.push((i + 1, ranges));
    }

    // The document's own contents, from the same file. The sender has every
    // byte and has already walked the page tree for the ranges above; numbering
    // the headings is that walk, once more — and it saves the client a walk of
    // its own, which is the page-tree traversal that costs a cold reader the
    // most. A document without an outline adds one header line.
    let toc = match File::load_data(&data[..], b"", ParseOptions::strict(), NoCache, NoCache, NoLog) {
        Ok(file) => pdf_web::outline::entries(&file.resolver(), file.get_root()),
        Err(e) => {
            eprintln!("{path}: no contents: {e}");
            Vec::new()
        }
    };

    let table = format_table(page_count, &pages, &toc);
    if let Err(e) = std::fs::write(&out, &table) {
        eprintln!("{out}: {e}");
        std::process::exit(1);
    }

    // What a client pays if it visits every page in order, cold: the union of
    // all ranges, so shared objects are not counted once per page.
    let mut all: Vec<Range<usize>> = pages.iter().flat_map(|(_, r)| r.iter().cloned()).collect();
    all.sort_by_key(|r| r.start);
    let mut union: Vec<Range<usize>> = Vec::new();
    for r in all {
        match union.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => union.push(r),
        }
    }
    let cold: usize = union.iter().map(|r| r.end - r.start).sum();
    let ranges: usize = pages.iter().map(|(_, r)| r.len()).sum();

    println!(
        "{path}: {page_count} page(s), {} heading(s), {} in the table, {ranges} range(s), {cold} byte(s) for a cold pass over every page, {:.1}s{}",
        toc.len(),
        table.len(),
        started.elapsed().as_secs_f32(),
        if failed > 0 { format!(", {failed} page(s) failed") } else { String::new() },
    );
    println!("  -> {out}");
}
