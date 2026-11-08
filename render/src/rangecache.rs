//! Rendering a document you have not downloaded yet.
//!
//! The viewer knows the share key and the page it wants to show, not the bytes.
//! Three pieces let it stay that way:
//!
//! * [`RangeCache`] — the bytes we do have, as sorted non-overlapping chunks.
//! * [`LazyReader`] — a `pdf::Backend` over a `RangeCache`. A read it cannot
//!   answer in full is answered with the bytes we do have from its start (often
//!   none) and the gap is *recorded*; the parser then either copes or fails,
//!   and we keep going with the next round.
//! * [`render_with_ranges`] — the loop: run, collect every miss, hand them to
//!   the fetcher **as one batch**, run again. Not one round trip per object.
//!
//! Two things make the batch worth it and one thing spoils it:
//!
//! * Worth it: the misses accumulate across a whole page, so a page costs one
//!   or two round trips, not one per font, image and XObject. A pass does not
//!   stop at the first object it cannot read — `pdf` answers `null` for an
//!   object whose bytes are not here (7.3.9), so one pass collects everything
//!   the page asks for that it does not have.
//! * Spoils it: the payload has to be addressable per range. A range of an
//!   AES-GCM-encrypted blob decrypts the whole blob; the bundle format has to
//!   be chunked first (64 KiB, nonce = counter, AAD = the header), otherwise
//!   "fetch only what is needed" buys nothing.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::ops::Range;
use std::rc::Rc;

use pdf::backend::{Backend as PdfBackend, IndexRange};
use pdf::error::Result;

/// Bytes of the document that are already here.
///
/// Chunks are kept sorted and coalesced, so a batch that arrives contiguous
/// costs one lookup. Lookups are binary search over chunk starts; a request
/// inside one chunk is served, one that straddles two is a miss.
#[derive(Default, Clone)]
pub struct RangeCache {
    /// (start, bytes), sorted by start, never overlapping.
    chunks: Vec<(usize, Vec<u8>)>,
    len: usize,
}

impl RangeCache {
    pub fn new(file_len: usize) -> Self {
        RangeCache { chunks: Vec::new(), len: file_len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Bytes held in memory.
    pub fn held(&self) -> usize {
        self.chunks.iter().map(|(_, d)| d.len()).sum()
    }

    /// Store `data` at `start`. Neighbouring chunks are merged.
    pub fn insert(&mut self, start: usize, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let end = start + data.len();
        // Where does it belong?
        let at = match self.chunks.binary_search_by_key(&start, |(s, _)| *s) {
            Ok(i) => {
                // Same start: keep the longer one.
                if self.chunks[i].1.len() >= data.len() {
                    return;
                }
                self.chunks[i].1 = data;
                i
            }
            Err(i) => {
                self.chunks.insert(i, (start, data));
                i
            }
        };
        self.coalesce_at(at);
    }

    /// Merge chunk `i` with its neighbour on the right, repeatedly.
    fn coalesce_at(&mut self, i: usize) {
        while i + 1 < self.chunks.len() {
            let (s0, d0) = &self.chunks[i];
            let (s1, _) = &self.chunks[i + 1];
            if *s0 + d0.len() != *s1 {
                break;
            }
            let (_, d1) = self.chunks.remove(i + 1);
            self.chunks[i].1.extend_from_slice(&d1);
        }
        // and with the one on the left
        if i > 0 {
            let (s0, d0) = &self.chunks[i - 1];
            let (s1, _) = &self.chunks[i];
            if *s0 + d0.len() == *s1 {
                let (_, d1) = self.chunks.remove(i);
                self.chunks[i - 1].1.extend_from_slice(&d1);
            }
        }
    }

    /// The contiguous bytes we hold starting at `from`, and the offset where
    /// they end.
    ///
    /// Chunks are coalesced, so what a read can be answered with before it runs
    /// into a gap is at most one chunk's tail.
    pub fn tail_from(&self, from: usize) -> (usize, &[u8]) {
        let i = match self.chunks.binary_search_by_key(&from, |(s, _)| *s) {
            Ok(i) => i,
            Err(0) => return (from, &[]),
            Err(i) => i - 1,
        };
        let (s, d) = &self.chunks[i];
        let off = from - *s;
        if off >= d.len() {
            (from, &[])
        } else {
            (from + d.len() - off, &d[off..])
        }
    }

    /// The bytes of `range`, if they are all here.
    pub fn get(&self, range: Range<usize>) -> Option<&[u8]> {
        if range.start == range.end {
            return Some(&[]);
        }
        let i = match self.chunks.binary_search_by_key(&range.start, |(s, _)| *s) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let (s, d) = &self.chunks[i];
        if range.start >= *s && range.end <= *s + d.len() {
            Some(&d[range.start - *s .. range.end - *s])
        } else {
            None
        }
    }

    /// Append the parts of `range` we do not have to `out`, in order.
    pub fn missing_into(&self, range: Range<usize>, out: &mut Vec<Range<usize>>) {
        let mut pos = range.start;
        // First chunk that could cover `pos`.
        let mut i = match self.chunks.binary_search_by_key(&pos, |(s, _)| *s) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        while pos < range.end {
            match self.chunks.get(i) {
                Some((s, d)) => {
                    let end = *s + d.len();
                    if end <= pos {
                        i += 1;
                        continue;
                    }
                    if *s > pos {
                        out.push(pos..(*s).min(range.end));
                    }
                    if end > pos {
                        pos = end;
                    }
                    i += 1;
                }
                None => break,
            }
        }
        if pos < range.end {
            out.push(pos..range.end);
        }
    }

    /// The parts of `wanted` we do not have: merged, sorted, coalesced.
    pub fn missing(&self, wanted: &[Range<usize>]) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        for r in wanted {
            self.missing_into(r.clone(), &mut out);
        }
        coalesce(out, 0)
    }
}

/// Sort, drop empty ranges, join ones that touch or are close.
///
/// A 4 KiB gap is not worth a second HTTP request; the slack is cheaper than
/// the round trip, and PDFs put objects next to each other anyway.
pub fn coalesce(mut ranges: Vec<Range<usize>>, max_gap: usize) -> Vec<Range<usize>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end + max_gap => {
                if r.end > last.end {
                    last.end = r.end;
                }
            }
            _ => out.push(r),
        }
    }
    out
}

/// Where the misses of a [`LazyReader`] land.
#[derive(Default)]
pub struct MissLog {
    ranges: RefCell<Vec<Range<usize>>>,
    /// Ranges that were asked for again with a bigger window.
    rounds: RefCell<u32>,
}

impl MissLog {
    pub fn is_empty(&self) -> bool {
        self.ranges.borrow().is_empty()
    }

    pub fn len(&self) -> usize {
        self.ranges.borrow().len()
    }

    /// The misses since the last call, merged into as few ranges as possible.
    pub fn take(&self) -> Vec<Range<usize>> {
        let taken = std::mem::take(&mut *self.ranges.borrow_mut());
        coalesce(taken, 4 * 1024)
    }

    fn record(&self, range: Range<usize>) {
        self.ranges.borrow_mut().push(range);
    }
}

/// A `pdf::Backend` that answers from a [`RangeCache`] and remembers what it
/// could not answer.
///
/// Open-ended reads (`pos..`) are **not** clamped. Since every object read is
/// bounded by the next offset in the cross-reference table, an open-ended read
/// is what `pdf` only does when nothing is known to follow it — the last
/// object, the final cross-reference section, the `%%EOF` marker — so it
/// already costs the tail of the document and nothing more. Clamping used to
/// protect against a parser slurping the file, and it made a trailing section
/// never converge: the fetched window was still too short, the range then
/// looked complete, and the loop could not tell that its own guess was the
/// problem.
pub struct LazyReader<'a> {
    cache: &'a RangeCache,
    len: usize,
    log: Rc<MissLog>,
}

impl<'a> LazyReader<'a> {
    pub fn new(cache: &'a RangeCache) -> (Self, Rc<MissLog>) {
        let log = Rc::new(MissLog::default());
        (
            LazyReader { cache, len: cache.len(), log: log.clone() },
            log,
        )
    }
}

impl<'a> PdfBackend for LazyReader<'a> {
    fn read<T: IndexRange>(&self, range: T) -> Result<&[u8]> {
        let r = range.to_range(self.len)?;
        match self.cache.get(r.clone()) {
            Some(data) => Ok(data),
            None => {
                // Not fatal: the caller gets the bytes we do have from
                // `r.start` (possibly none) and the pass fails or copes. It is
                // rerun from the top once the fetch has filled the gap.
                let (end, have) = self.cache.tail_from(r.start);
                debug_assert!(end < r.end, "get() failed but the range is here");
                self.log.record(end..r.end);
                *self.log.rounds.borrow_mut() += 1;
                Ok(have)
            }
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// A read is incomplete exactly when the cache can not answer its whole
    /// range; `read` returned the prefix it has. `Storage::resolve_ref` asks
    /// this after a parse fails, and calls the object null rather than broken
    /// so the pass keeps walking.
    fn is_incomplete(&self, range: Range<usize>) -> bool {
        self.cache.get(range).is_none()
    }
}

/// A `pdf::Backend` over bytes we already have that writes down every read.
///
/// This is the sender's side of a per-page range table: whoever holds the whole
/// file — the machine that builds a share, a CDN origin, a proxy — renders a
/// page once against this and gets exactly the ranges a client that has nothing
/// else will ask for. Nothing is guessed and nothing is missing, so the client
/// needs one round trip per page instead of a discovery loop.
pub struct Recorder<'a> {
    data: &'a [u8],
    log: RefCell<Vec<Range<usize>>>,
}

impl<'a> Recorder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Recorder { data, log: RefCell::new(Vec::new()) }
    }

    /// The ranges read so far — merged, sorted, with `max_gap` slack so a
    /// table does not split into ranges a few hundred bytes apart — and a
    /// reset, so one recorder can serve every page in turn.
    pub fn take(&self, max_gap: usize) -> Vec<Range<usize>> {
        coalesce(std::mem::take(&mut *self.log.borrow_mut()), max_gap)
    }
}

impl PdfBackend for Recorder<'_> {
    fn read<T: IndexRange>(&self, range: T) -> Result<&[u8]> {
        let r = range.to_range(self.data.len())?;
        self.log.borrow_mut().push(r.clone());
        Ok(&self.data[r])
    }

    fn len(&self) -> usize {
        self.data.len()
    }
}

/// A per-page range table as text, the thing a client fetches once per bundle.
///
/// ```text
/// pdfranges 1
/// 169
/// 5 12 0 4096 8192 2048 …
/// ```
///
/// Line 1 is the format tag and its version; line 2 is a hint at how many pages
/// the document has; then one line per page that has a table: the 1-based page
/// number, how many ranges follow, and `start length` for each. A page with no
/// line is simply not covered, and the client falls back to discovering it.
/// Page 0 is the document itself — what has to be read to know how many pages
/// there are — so a client can start with one request instead of four.
/// Numbers are decimal and ranges are disjoint, ascending and already merged.
///
/// After the page lines may come the document's table of contents:
///
/// ```text
/// toc 2
/// 0 - Foreword
/// 1 5 Editing PDFs
/// ```
///
/// `<depth> <1-based page or -> <title>`. Whoever builds the table has the whole
/// file open and already walks the page tree, so resolving the outline against
/// that walk costs almost nothing here — and the client then needs no page-tree
/// walk of its own to number the headings in its sidebar. A reader that does
/// not know the section skips it: `toc` does not parse as a page line, and the
/// lines under it need a title field to be one. That is why this did not need a
/// new format tag.
pub fn format_table(
    page_count: u32,
    pages: &[(u32, Vec<Range<usize>>)],
    toc: &[TocEntry],
) -> String {
    let mut out = String::with_capacity(64 * (pages.len() + toc.len()).max(1));
    out.push_str("pdfranges 1\n");
    let _ = writeln!(out, "{page_count}");
    for (page, ranges) in pages {
        let _ = write!(out, "{page} {}", ranges.len());
        for r in ranges {
            let _ = write!(out, " {} {}", r.start, r.end - r.start);
        }
        out.push('\n');
    }
    if !toc.is_empty() {
        let _ = writeln!(out, "toc {}", toc.len());
        for entry in toc {
            match entry.page {
                Some(page) => {
                    let _ = write!(out, "{} {} ", entry.depth, page + 1);
                }
                None => {
                    let _ = write!(out, "{} - ", entry.depth);
                }
            }
            // A title is the rest of the line, so it can hold spaces; a break
            // in it would end the row early.
            for c in entry.title.chars() {
                out.push(if c.is_control() { ' ' } else { c });
            }
            out.push('\n');
        }
    }
    out
}

/// One row of a document's table of contents, as a table file carries it.
///
/// `page` is 0-based, the numbering the viewer's `?page=` uses; `None` is a
/// heading that points out of the document (another file, a script). Such a row
/// is kept: dropping it would silently empty a chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    pub depth: u8,
    pub page: Option<u32>,
    pub title: String,
}

/// A borrowed backend.
///
/// `pdf::Backend` can not be implemented for `&B` here: the blanket impl for
/// `Deref<Target = [u8]>` would collide with it. One line of forwarding gives
/// the callers a name for the borrow instead.
pub struct Borrowed<'a, B: ?Sized>(pub &'a B);

impl<B: PdfBackend + ?Sized> PdfBackend for Borrowed<'_, B> {
    fn read<T: IndexRange>(&self, range: T) -> Result<&[u8]> {
        self.0.read(range)
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn is_incomplete(&self, range: Range<usize>) -> bool {
        self.0.is_incomplete(range)
    }
}

/// What one attempted pass produced.
pub enum Pass<T> {
    /// Everything it needed was there.
    Done(T),
    /// These ranges are missing; fetch them and call again.
    ///
    /// `err` is what the pass failed with, for the case where the fetched data
    /// changes nothing and we have to give up on the last error.
    Need { ranges: Vec<Range<usize>>, err: Option<String> },
}

/// Run `pass` until it stops asking for bytes, fetching each round's misses as
/// one batch.
///
/// `fetch` is where the network lives. It gets the whole list at once and must
/// put the answers into `cache` before returning. `pass` is rerun from the top
/// each round; that is cheap because the parse is bounded by the page.
///
/// A round count of `max_rounds` (4 is generous) guards against a document
/// that keeps finding new objects forever.
pub fn render_with_ranges<T>(
    cache: &mut RangeCache,
    max_rounds: usize,
    mut pass: impl FnMut(&LazyReader) -> Result<T>,
    mut fetch: impl FnMut(&mut RangeCache, &[Range<usize>]) -> std::result::Result<(), String>,
) -> std::result::Result<T, String> {
    let mut last_err: Option<String> = None;
    for _ in 0..max_rounds {
        let (reader, log) = LazyReader::new(cache);
        let outcome = pass(&reader);
        let misses = log.take();
        drop(reader);
        match outcome {
            Ok(t) if misses.is_empty() => return Ok(t),
            Ok(_) => {}
            Err(e) => last_err = Some(e.to_string()),
        }
        // Only ask for what is not here: a round that needs an object again,
        // or one that overlaps a range already fetched, costs nothing.
        let misses = cache.missing(&misses);
        if misses.is_empty() {
            // Nothing left to fetch, so the failure was not about the bytes.
            return Err(last_err.unwrap_or_else(|| "render failed".into()));
        }
        fetch(cache, &misses)?;
    }
    Err(last_err.unwrap_or_else(|| format!("gave up after {max_rounds} rounds")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_coalesces_and_get_spans() {
        let mut c = RangeCache::new(100);
        c.insert(10, vec![1, 2, 3]);
        c.insert(13, vec![4, 5]);
        assert_eq!(c.held(), 5);
        assert_eq!(c.get(10..15), Some(&[1u8, 2, 3, 4, 5][..]));
        assert_eq!(c.get(11..15), Some(&[2u8, 3, 4, 5][..]));
        assert_eq!(c.get(9..12), None);
        c.insert(0, vec![0; 5]);
        assert_eq!(c.held(), 10);
        assert_eq!(c.get(0..6), None);
        assert_eq!(c.get(0..5), Some(&[0u8; 5][..]));
    }

    #[test]
    fn missing_reports_holes_only() {
        let mut c = RangeCache::new(100);
        c.insert(10, vec![0; 10]); // 10..20
        c.insert(40, vec![0; 10]); // 40..50
        assert_eq!(c.missing(&[0..100]), vec![0..10, 20..40, 50..100]);
        assert_eq!(c.missing(&[12..18]), Vec::<Range<usize>>::new());
        assert_eq!(c.missing(&[15..45]), vec![20..40]);
    }

    #[test]
    fn coalesce_joins_close_ranges() {
        assert_eq!(coalesce(vec![10..20, 22..30, 100..110], 4), vec![10..30, 100..110]);
        assert_eq!(coalesce(vec![10..20, 30..40], 4), vec![10..20, 30..40]);
        assert_eq!(coalesce(vec![10..20, 12..15, 10..30], 0), vec![10..30]);
    }

    #[test]
    fn open_ended_reads_ask_for_the_rest_of_the_file() {
        let mut c = RangeCache::new(1_000_000);
        c.insert(5000, vec![7; 100]);
        let (reader, log) = LazyReader::new(&c);
        assert_eq!(reader.read(5000..5100).unwrap(), &[7u8; 100][..]);
        assert!(!reader.is_incomplete(5000..5100));
        // 5000.. wants everything from 5000 to the end, which we do not have:
        // the read is served with the 100 bytes that are here and the rest of
        // the file is asked for.
        assert_eq!(reader.read(5000..).unwrap(), &[7u8; 100][..]);
        assert!(reader.is_incomplete(5000..1_000_000));
        assert_eq!(log.take(), vec![5100..1_000_000]);
        assert!(reader.read(6000..).unwrap().is_empty());
        assert_eq!(log.take(), vec![6000..1_000_000]);
    }

    #[test]
    fn partial_hit_serves_the_prefix_and_asks_for_the_rest() {
        let mut c = RangeCache::new(1_000_000);
        c.insert(100, vec![1; 10]);
        let (reader, log) = LazyReader::new(&c);
        // 100..200 straddles the end of the chunk: the parser gets ten bytes.
        assert_eq!(reader.read(100..200).unwrap(), &[1u8; 10][..]);
        assert!(reader.is_incomplete(100..200));
        assert_eq!(log.take(), vec![110..200]);
        // Once the gap is filled the same read is answered in full.
        drop(reader);
        c.insert(110, vec![2; 90]);
        let (reader, log) = LazyReader::new(&c);
        assert_eq!(reader.read(100..200).unwrap().len(), 100);
        assert!(!reader.is_incomplete(100..200));
        assert!(log.is_empty());
    }

    #[test]
    fn a_table_is_written_and_read_the_same_way() {
        let table = format_table(3, &[(1, vec![0..10, 20..40]), (3, vec![4096..8192])], &[]);
        assert_eq!(table, "pdfranges 1\n3\n1 2 0 10 20 20\n3 1 4096 4096\n");
    }

    #[test]
    fn the_contents_rides_along_with_the_table() {
        let toc = vec![
            TocEntry { depth: 0, page: None, title: "Foreword".into() },
            TocEntry { depth: 1, page: Some(4), title: "Edit\ning PDFs".into() },
        ];
        let table = format_table(9, &[(1, vec![0..10])], &toc);
        assert_eq!(
            table,
            "pdfranges 1\n9\n1 1 0 10\ntoc 2\n0 - Foreword\n1 5 Edit ing PDFs\n"
        );
    }

    #[test]
    fn a_recorder_writes_down_every_read() {
        let data = vec![0u8; 100];
        let rec = Recorder::new(&data);
        assert_eq!(rec.read(10..20).unwrap().len(), 10);
        assert_eq!(rec.read(22..30).unwrap().len(), 8);
        // Coalesced with slack: a table does not split into ranges 200 bytes apart.
        assert_eq!(rec.take(4 * 1024), vec![10..30]);
        assert!(rec.take(4 * 1024).is_empty());
    }

    #[test]
    fn a_pass_keeps_asking_after_an_object_it_could_not_read() {
        // The behaviour the round loop depends on: a read that comes back
        // empty does not stop the caller, so one pass collects every range it
        // was missing. (The log merges misses that are close together, which
        // is why the two become one range.)
        let mut c = RangeCache::new(64);
        c.insert(0, b"%PDF-1.7\n".to_vec());
        let (reader, log) = LazyReader::new(&c);
        assert!(reader.read(10..20).unwrap().is_empty());
        assert!(reader.is_incomplete(10..20));
        assert_eq!(reader.read(30..40).unwrap(), &b""[..]);
        assert_eq!(log.take(), vec![10..40]);
    }
}
