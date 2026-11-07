//! `pdf_render` in the browser.
//!
//! A page is fetched by range, not downloaded: the host asks this module what
//! a page needs, fetches exactly that, hands it back, and asks again. That is
//! one round trip per pass, and two passes are the normal cost of a page —
//! one, if the sender also hands out the per-page table of ranges it computed
//! when it published the file (see `render/examples/net_sim.rs` for what a
//! table is worth).
//!
//! The ABI is deliberately plain: exported `extern "C"` functions over
//! `WebAssembly.Memory`, with **no imports at all**. That leaves the host
//! nothing to audit but the export list — `WebAssembly.Module.imports(mod)`
//! is empty — and the module cannot reach the network, the DOM or `eval`
//! even if it wanted to. Everything that touches the outside world (fetch,
//! <img>, the font bundle) is JavaScript.
//!
//! ```text
//! doc = pdf_doc_new(file_len)
//! while (needs = pdf_page_needs(doc, page)) is not empty:
//!     await fetch each range with a Range header, in parallel
//!     pdf_supply(doc, offset, bytes, len) for each
//! svg = pdf_page_svg(doc, page)      // one self-contained SVG document
//! ```

use std::ops::Range;

use pdf_render::pdf::backend::{Backend as PdfBackend, IndexRange};
use pdf_render::pdf::error::Result as PdfResult;
use pdf_render::pdf::file::{File, NoCache, NoLog};
use pdf_render::pdf::object::ParseOptions;
use pdf_render::rangecache::{coalesce, LazyReader, RangeCache};
use pdf_render::svg_backend::{OutlineBuilder, SvgBackend};
use pdf_render::{render_page, Cache, MemDir, StandardCache, Transform2F};

mod abi;
mod svg;

#[cfg(test)]
mod tests;

/// Not a page number: opening the document is all this asks for.
pub const OPEN_ONLY: u32 = u32::MAX;

/// How far a missing range is widened on both sides, in bytes. Small enough
/// that a guess costs little on a slow link, large enough to swallow the rest
/// of the object that caused it and its neighbours.
const FETCH_SLACK: usize = 16 * 1024;

/// What the host should do next.
pub enum Ready {
    /// Fetch these, supply them, ask again. Sorted, merged, disjoint.
    Need(Vec<Range<usize>>),
    /// The page is in `Doc::ready`; take it with [`Doc::take_svg`].
    Page,
    /// `OPEN_ONLY` succeeded: the document has this many pages.
    Opened(u32),
    /// Nothing is missing and it still does not render: [`Doc::error`].
    Failed,
}

/// A `pdf::Backend` over the borrowed reader.
///
/// `Backend` cannot be implemented for `&B` (that collides with the blanket
/// impl for `Deref<Target = [u8]>`), so the borrow gets a type of its own.
struct Borrowed<'a, 'b>(&'a LazyReader<'b>);

impl PdfBackend for Borrowed<'_, '_> {
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

#[derive(Debug)]
enum Passed {
    Opened(u32),
    Page(String),
}

/// One document, and the bytes of it that have arrived.
pub struct Doc {
    cache: RangeCache,
    render: Cache<OutlineBuilder>,
    ready: Option<(u32, String)>,
    opened: Option<u32>,
    error: Option<String>,
    /// The last list handed out. A pass that asks for the same bytes twice has
    /// not lost anything to the network, so it must not loop.
    last_need: Vec<Range<usize>>,
}

impl Doc {
    /// `file_len` is the whole size of the document, known before anything is
    /// fetched (from a `HEAD`, or a `Content-Length` on the first response).
    /// Every range is checked against it, so a truncated or wrong length shows
    /// up as a missing range, not as a parse error halfway in.
    pub fn new(file_len: u32) -> Doc {
        Doc {
            cache: RangeCache::new(file_len as usize),
            render: Cache::new(OutlineBuilder {}),
            ready: None,
            opened: None,
            error: None,
            last_need: Vec::new(),
        }
    }

    /// Store bytes that arrived for `offset`.
    ///
    /// Overlap is fine: the cache keeps the union, so a host that widens a
    /// request (or answers a range with the whole file) costs nothing.
    pub fn supply(&mut self, offset: u32, data: &[u8]) {
        if offset as usize >= self.cache.len() {
            return;
        }
        let room = self.cache.len() - offset as usize;
        self.cache.insert(offset as usize, data[..data.len().min(room)].to_vec());
    }

    /// Hand in the standard fonts: the bundle `fonts_blob` writes.
    ///
    /// Not optional and not lazy — the browser fetches the whole set before
    /// the first page, so which fonts a document uses cannot be observed from
    /// the outside.
    pub fn set_fonts(&mut self, bundle: &[u8]) -> Result<(), String> {
        let dir = MemDir::parse_bundle(bundle).map_err(|e| e.to_string())?;
        self.render.set_standard(StandardCache::with_dir(dir));
        Ok(())
    }

    pub fn num_pages(&self) -> u32 {
        self.opened.unwrap_or(0)
    }

    /// Run one pass over the document.
    ///
    /// A pass is deliberately not retried here: the bytes are somewhere else,
    /// and only the host can go and get them. So this says what to fetch and
    /// is called again after the fetch, until it stops asking.
    pub fn prepare(&mut self, page: u32) -> Ready {
        self.ready = None;
        let (out, misses) = self.pass(page);
        let missing = self.cache.missing(&misses);

        if !missing.is_empty() {
            // Each miss is widened before it is handed out. One object per
            // round trip is what a page costs otherwise: a pass stops where it
            // cannot read — a page tree node, a content stream, a font — and
            // the next object is only learned from the next pass. The bytes
            // next to a missing object are usually needed too, so a wider
            // window buys several rounds for a few KiB.
            let len = self.cache.len();
            let missing = coalesce(
                missing
                    .into_iter()
                    .map(|r| r.start.saturating_sub(FETCH_SLACK)..(r.end + FETCH_SLACK).min(len))
                    .collect(),
                0,
            );
            let missing = self.cache.missing(&missing);
            if missing.is_empty() {
                self.error = Some("a range was widened into bytes already held".into());
                return Ready::Failed;
            }
            if missing == self.last_need {
                self.error = Some(format!(
                    "the document asked for the same {} range(s) twice",
                    missing.len()
                ));
                return Ready::Failed;
            }
            self.last_need = missing.clone();
            return Ready::Need(missing);
        }

        match out {
            Ok(Passed::Opened(n)) => {
                self.opened = Some(n);
                self.error = None;
                Ready::Opened(n)
            }
            Ok(Passed::Page(svg)) => {
                self.ready = Some((page, svg));
                self.error = None;
                Ready::Page
            }
            Err(e) => {
                self.error = Some(e);
                Ready::Failed
            }
        }
    }

    /// The page `prepare` just rendered, if it is for `page`.
    pub fn take_svg(&mut self, page: u32) -> Option<String> {
        match self.ready.take() {
            Some((p, svg)) if p == page => Some(svg),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Record a failure that happened outside a pass, e.g. a damaged bundle.
    pub fn fail(&mut self, e: String) {
        self.error = Some(e);
    }

    /// Bytes of the document held in memory.
    pub fn held(&self) -> usize {
        self.cache.held()
    }

    fn pass(&mut self, page: u32) -> (Result<Passed, String>, Vec<Range<usize>>) {
        // Both fields are borrowed at once, and they are different fields:
        // the reader sees the bytes, the cache sees the fonts.
        let Doc { cache, render, .. } = self;
        let (reader, log) = LazyReader::new(cache);
        let out = (|| {
            let file = File::load_data(
                Borrowed(&reader),
                b"",
                ParseOptions::strict(),
                NoCache,
                NoCache,
                NoLog,
            )
            .map_err(|e| e.to_string())?;
            if page == OPEN_ONLY {
                return Ok(Passed::Opened(file.num_pages()));
            }
            let page_obj = file.get_page(page).map_err(|e| e.to_string())?;
            let resolver = file.resolver();
            let mut backend = SvgBackend::new(render);
            render_page(&mut backend, &resolver, &page_obj, Transform2F::default())
                .map_err(|e| e.to_string())?;
            Ok(Passed::Page(svg::combine(backend.finish())))
        })();
        drop(reader);
        (out, log.take())
    }
}
