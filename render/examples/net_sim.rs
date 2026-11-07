//! A simulated network in front of the range-fetching viewer.
//!
//! usage: cargo run --features svg --example net_sim -- <file.pdf> [pages] [options]
//!
//!   pages         "1-3", "1,4,9" or "1-3,7" (default: the first three)
//!   --head KiB    bytes carried by the first response (default 16)
//!   --tail KiB    ... and from the end of the file (default 4)
//!   --slack KiB   how far a missed range is widened (default 64)
//!   --warm        keep one cache across the pages instead of a fresh one per
//!                 page (default: cold, so each page's numbers stand alone)
//!   --verbose     print every request
//!
//! Two questions matter before this goes over a real link: how many round
//! trips a page costs, and how many bytes of a document that page actually
//! pulls. Both strategies are measured per page, from the same starting point:
//!
//! * **discovery** — the client asks only for what a failed pass recorded,
//!   widened by `--slack`, and repeats until nothing is missing. This is what
//!   works against a server that answers ranges but knows nothing about PDFs.
//! * **table** — the sender computed the ranges that page needs and hands them
//!   out, as a linearization hint would. One request, then the render should
//!   miss nothing; if it does, that is reported, because it means the table
//!   was incomplete, not that the client was slow.
//!
//! The table is produced here by rendering the page once from the whole file
//! with a backend that records every read — which is exactly what a sender
//! holding the file can do offline, once, when it builds the share.
//!
//! A request here is one round trip that may carry several ranges; `ranges`
//! counts the ranges that travelled in it. Nothing is cached across runs, so
//! the bytes are the transfer a single page costs, not what a CDN would see.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use pdf::backend::{Backend, IndexRange};
use pdf::error::Result as PdfResult;
use pdf::file::{File, NoCache, NoLog};
use pdf::object::ParseOptions;
use pdf_render::rangecache::{coalesce, render_with_ranges, LazyReader, RangeCache};
use pdf_render::svg_backend::{Layer, OutlineBuilder, SvgBackend};
use pdf_render::{render_page, Cache};
use pathfinder_geometry::transform2d::Transform2F;

/// What the network did.
#[derive(Clone, Copy, Default, Debug)]
struct Counters {
    /// Round trips. A request may carry several ranges.
    requests: usize,
    ranges: usize,
    bytes: usize,
}

impl std::ops::AddAssign for Counters {
    fn add_assign(&mut self, o: Counters) {
        self.requests += o.requests;
        self.ranges += o.ranges;
        self.bytes += o.bytes;
    }
}

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

/// A backend that has the whole file and writes down what was read.
///
/// This is the sender's side: run the page once against the file it already
/// has, and every range it touched is a range the client will need.
struct Recording<'a> {
    data: &'a [u8],
    log: Rc<RefCell<Vec<Range<usize>>>>,
}

impl Backend for Recording<'_> {
    fn read<T: IndexRange>(&self, range: T) -> PdfResult<&[u8]> {
        let r = range.to_range(self.data.len())?;
        self.log.borrow_mut().push(r.clone());
        Ok(&self.data[r])
    }
    fn len(&self) -> usize {
        self.data.len()
    }
}

/// What one render produced, in a form two runs can be compared on: the
/// vector layers' bytes, and how many images are on the page. Raster bytes are
/// not part of the page's SVG — they travel separately — so counting them is
/// the only way to know the page had them at all.
#[derive(Clone, PartialEq, Debug)]
struct Rendered {
    vector_layers: usize,
    svg_bytes: usize,
    images: usize,
    image_bytes: usize,
}

/// Render one page from whatever backend is given, and say what came out.
fn render_one<B: Backend>(backend: B, page_nr: u32) -> PdfResult<Rendered> {
    let file = File::load_data(backend, b"", ParseOptions::strict(), NoCache, NoCache, NoLog)?;
    let page = file.get_page(page_nr - 1)?;
    let resolver = file.resolver();
    let mut glyphs = Cache::new(OutlineBuilder::default());
    let mut backend = SvgBackend::new(&mut glyphs);
    render_page(&mut backend, &resolver, &page, Transform2F::default())?;
    let page = backend.finish();
    let mut out = Rendered { vector_layers: 0, svg_bytes: 0, images: 0, image_bytes: 0 };
    for layer in &page.layers {
        match layer {
            Layer::Vector(svg) => {
                out.vector_layers += 1;
                out.svg_bytes += svg.len();
            }
            Layer::Image(image) => {
                out.images += 1;
                out.image_bytes += image.rgba.len();
            }
        }
    }
    Ok(out)
}

/// The "network": the file, and a policy for what a request carries.
struct Sim {
    server: Vec<u8>,
    head: usize,
    tail: usize,
    slack: usize,
    verbose: bool,
}

impl Sim {
    fn len(&self) -> usize {
        self.server.len()
    }

    /// A cache that has had the initial response, and what that response cost.
    fn seeded(&self) -> (RangeCache, Counters) {
        let len = self.len();
        let head = len.min(self.head);
        let tail = len.saturating_sub(head).min(self.tail);
        let mut wanted = Vec::new();
        if head > 0 {
            wanted.push(0..head);
        }
        if tail > 0 {
            wanted.push(len - tail..len);
        }
        let mut cache = RangeCache::new(len);
        let mut n = Counters::default();
        self.request(&mut cache, &wanted, &mut n);
        (cache, n)
    }

    /// One round trip: ask for `wanted`, take delivery into `cache`.
    ///
    /// Ranges we already hold are dropped first, so a request never asks for
    /// bytes it has, and the byte count is what actually crossed the wire.
    fn request(&self, cache: &mut RangeCache, wanted: &[Range<usize>], n: &mut Counters) {
        let wanted = cache.missing(wanted);
        if wanted.is_empty() {
            return;
        }
        n.requests += 1;
        n.ranges += wanted.len();
        let mut bytes = 0;
        for r in &wanted {
            n.bytes += r.len();
            bytes += r.len();
            cache.insert(r.start, self.server[r.clone()].to_vec());
        }
        if self.verbose {
            println!("      one request: {} range(s), {} bytes", wanted.len(), bytes);
        }
    }

    /// The ranges page `page_nr` reads when nothing is missing.
    ///
    /// Merged with a little slack: a sender would not split a table into
    /// ranges 200 bytes apart, and every range costs a byte range header.
    fn table_for(&self, page_nr: u32) -> Result<Vec<Range<usize>>, String> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let backend = Recording { data: &self.server, log: log.clone() };
        render_one(backend, page_nr).map_err(|e| e.to_string())?;
        let reads = std::mem::take(&mut *log.borrow_mut());
        Ok(coalesce(reads, 4 * 1024))
    }

    /// Fetch the sender's table, then render. Rounds should be zero.
    ///
    /// Also returns the size of the table itself, which has to travel: it is
    /// counted as if it arrived with the first response, like a linearization
    /// hint written into the file.
    fn via_table(
        &self,
        cache: &mut RangeCache,
        page_nr: u32,
        n: &mut Counters,
    ) -> Result<(Rendered, usize, usize), String> {
        let table = self.table_for(page_nr)?;
        let wire = table_wire_bytes(&table);
        n.bytes += wire;
        self.request(cache, &table, n);
        let mut rounds = 0;
        let out = render_with_ranges(
            cache,
            4,
            |reader| render_one(Borrowed(reader), page_nr),
            |cache, missing| {
                rounds += 1;
                // The table was short. Ask for the difference, still batched.
                self.request(cache, missing, n);
                Ok(())
            },
        )?;
        Ok((out, rounds, wire))
    }

    /// Ask only for what the previous pass recorded, until nothing is missing.
    fn discover(
        &self,
        cache: &mut RangeCache,
        page_nr: u32,
        n: &mut Counters,
    ) -> Result<(Rendered, usize, usize), String> {
        let slack = self.slack;
        let len = self.len();
        let mut rounds = 0;
        let out = render_with_ranges(
            cache,
            12,
            |reader| render_one(Borrowed(reader), page_nr),
            |cache, missing| {
                rounds += 1;
                // Each miss costs a round trip, so make it worth more than the
                // object that caused it: an object's neighbours are usually
                // needed too. This is the client's own guess.
                let widened: Vec<Range<usize>> = missing
                    .iter()
                    .map(|r| r.start.saturating_sub(slack)..(r.end + slack).min(len))
                    .collect();
                self.request(cache, &widened, n);
                Ok(())
            },
        )?;
        // No table and nothing to describe: the third place is the table's own
        // wire size, which discovery does not have.
        Ok((out, rounds, 0))
    }
}

/// Bytes a range list takes on the wire in the obvious encoding: how many
/// ranges, then for each a varint gap from the end of the previous one and a
/// varint length. A JSON list would be several times this.
fn table_wire_bytes(table: &[Range<usize>]) -> usize {
    let mut prev = 0;
    let mut bytes = varint_len(table.len());
    for r in table {
        bytes += varint_len(r.start.saturating_sub(prev)) + varint_len(r.end - r.start);
        prev = r.end;
    }
    bytes
}

fn varint_len(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

/// `1024` -> `1.0 KiB`.
fn kib(n: usize) -> String {
    if n < 10 * 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}

fn usage() {
    eprintln!("usage: net_sim <file.pdf> [pages] [--head KiB] [--tail KiB] [--slack KiB] [--warm] [--verbose]");
}

/// "1-3,7" -> [1, 2, 3, 7]
fn parse_pages(spec: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                let a: u32 = a.trim().parse().map_err(|_| format!("bad page {part:?}"))?;
                let b: u32 = b.trim().parse().map_err(|_| format!("bad page {part:?}"))?;
                if a == 0 || b < a || b - a > 512 {
                    return Err(format!("bad page range {part:?}"));
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse().map_err(|_| format!("bad page {part:?}"))?),
        }
    }
    if out.is_empty() {
        return Err("no pages".into());
    }
    Ok(out)
}

fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "error");
    }
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let mut path: Option<String> = None;
    let mut spec: Option<String> = None;
    let (mut head_kib, mut tail_kib, mut slack_kib) = (16usize, 4usize, 64usize);
    let (mut cold, mut verbose) = (true, false);

    let mut i = 1;
    while i < args.len() {
        let a = args[i].clone();
        let a = a.as_str();
        match a {
            "--head" | "--tail" | "--slack" => {
                i += 1;
                let v: usize = match args.get(i).and_then(|v| v.parse().ok()) {
                    Some(v) => v,
                    None => {
                        eprintln!("{a} wants a number of KiB");
                        return;
                    }
                };
                match a {
                    "--head" => head_kib = v,
                    "--tail" => tail_kib = v,
                    _ => slack_kib = v,
                }
            }
            "--cold" => cold = true,
            "--warm" => cold = false,
            "--verbose" => verbose = true,
            "-h" | "--help" => {
                usage();
                return;
            }
            _ if a.starts_with("--") => {
                eprintln!("unknown option {a}");
                usage();
                return;
            }
            _ => {
                if path.is_none() {
                    path = Some(a.to_owned());
                } else if spec.is_none() {
                    spec = Some(a.to_owned());
                } else {
                    eprintln!("unexpected argument {a}");
                    usage();
                    return;
                }
            }
        }
        i += 1;
    }
    let Some(path) = path else {
        usage();
        return;
    };

    if std::env::var_os("STANDARD_FONTS").is_none() {
        let local = if std::path::Path::new("fonts").is_dir() { "fonts" } else { "standard_fonts" };
        if std::path::Path::new(local).is_dir() {
            std::env::set_var("STANDARD_FONTS", local);
        }
    }

    let server = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(1)
    });
    let len = server.len();

    // How many pages there are: cheap, it only parses the trailer and the
    // page tree, and it is the sender's own copy.
    let page_count = match File::load_data(&server[..], b"", ParseOptions::strict(), NoCache, NoCache, NoLog) {
        Ok(f) => f.num_pages(),
        Err(e) => {
            eprintln!("{path}: cannot open: {e}");
            std::process::exit(1);
        }
    };

    let pages = match spec.as_deref().map(parse_pages).transpose() {
        Err(e) => {
            eprintln!("{e}");
            return;
        }
        Ok(None) => (1..=page_count.min(3)).collect::<Vec<u32>>(),
        Ok(Some(p)) => p,
    };
    if let Some(bad) = pages.iter().find(|p| **p == 0 || **p > page_count) {
        eprintln!("page {bad} is not in a document of {page_count} page(s)");
        return;
    }

    let sim = Sim {
        server,
        head: head_kib * 1024,
        tail: tail_kib * 1024,
        slack: slack_kib * 1024,
        verbose,
    };

    println!(
        "{path}: {} bytes, {page_count} page(s), measuring {}",
        kib(len),
        pages.len()
    );
    println!(
        "initial response: head {} + tail {} ({} bytes), slack {}{}",
        kib(sim.head),
        kib(sim.tail),
        sim.head.min(len) + sim.tail.min(len.saturating_sub(sim.head)),
        kib(sim.slack),
        if cold { ", cold: a fresh cache per page" } else { ", one cache across pages" },
    );
    println!();

    // In warm mode these are kept across pages, so what page 1 fetched for its
    // fonts is not paid for again by page 2. In cold mode they are replaced per
    // page, which is what makes the per-page numbers comparable.
    let (mut disc_cache, disc_seed) = sim.seeded();
    let (mut table_cache, table_seed) = sim.seeded();
    let mut total_disc = Counters::default();
    let mut total_table = Counters::default();
    if !cold {
        total_disc = disc_seed;
        total_table = table_seed;
        println!(
            "warm: the initial response is one request of {} for both strategies, counted once in the totals below",
            kib(disc_seed.bytes)
        );
    } else {
        println!("cold: each page's numbers below include its own initial response");
    }

    println!(
        "{:<5} {:<10} {:>8} {:>7} {:>9} {:>8} {:>7}",
        "page", "how", "requests", "ranges", "bytes", "of file", "rounds"
    );
    let mut failed = 0;
    let mut worst_page = (0u32, 0usize);

    for &page in &pages {
        let mut n_table = Counters::default();
        let mut n_disc = Counters::default();
        if cold {
            let (c, n) = sim.seeded();
            table_cache = c;
            n_table += n;
            let (c, n) = sim.seeded();
            disc_cache = c;
            n_disc += n;
        }

        let t = sim.via_table(&mut table_cache, page, &mut n_table);
        let d = sim.discover(&mut disc_cache, page, &mut n_disc);

        for (how, res, n) in [("table", &t, n_table), ("discovery", &d, n_disc)] {
            match res {
                Ok((out, rounds, _wire)) => println!(
                    "{:<5} {:<10} {:>8} {:>7} {:>9} {:>7.1}% {:>7} {}",
                    page,
                    how,
                    n.requests,
                    n.ranges,
                    kib(n.bytes),
                    100.0 * n.bytes as f64 / len as f64,
                    rounds,
                    if out.images > 0 {
                        format!("{} image(s), {}", out.images, kib(out.image_bytes))
                    } else {
                        String::new()
                    }
                ),
                Err(e) => {
                    println!("{:<5} {:<10} failed: {e}", page, how);
                    failed += 1;
                }
            }
        }
        total_table += n_table;
        total_disc += n_disc;
        if n_disc.bytes > worst_page.1 {
            worst_page = (page, n_disc.bytes);
        }
        if let (Ok((a, _, _)), Ok((b, _, _))) = (&t, &d) {
            if a != b {
                println!("      note: the two strategies rendered page {page} differently: {a:?} vs {b:?}");
            }
        }
        if !cold && verbose {
            println!();
        }
    }

    println!();
    println!(
        "table     : {} request(s), {} range(s), {} in total over {} page(s) — {:.2}x the file",
        total_table.requests,
        total_table.ranges,
        kib(total_table.bytes),
        pages.len(),
        total_table.bytes as f64 / len as f64
    );
    println!(
        "discovery : {} request(s), {} range(s), {} in total over {} page(s) — {:.2}x the file",
        total_disc.requests,
        total_disc.ranges,
        kib(total_disc.bytes),
        pages.len(),
        total_disc.bytes as f64 / len as f64
    );
    let overhead = |a: usize, b: usize| if b == 0 { 1.0 } else { a as f64 / b as f64 };
    println!(
        "discovery costs {:.1}x the requests and {:.2}x the bytes of the table",
        overhead(total_disc.requests, total_table.requests),
        overhead(total_disc.bytes, total_table.bytes)
    );
    if worst_page.1 > 0 {
        println!("worst page: {}, {} (discovery) of {}", worst_page.0, kib(worst_page.1), kib(len));
    }
    if failed > 0 {
        println!("{failed} page/strategy run(s) failed");
    }
    let per_page = if pages.is_empty() { 0.0 } else { total_disc.bytes as f64 / pages.len() as f64 };
    println!("discovery average: {} per page", kib(per_page as usize));
}
