//! Linearize a PDF (Annex F): the sender's side of "page 1 first".
//!
//! usage: cargo run --bin linearize -- <in.pdf> <out.pdf>
//!
//! Layout, in the order a reader that only wants page 1 should meet it:
//!
//! ```text
//! %PDF-1.7
//! <linearization dict>          /L /T /E /O /N
//! <first-page xref stream>      trailer: /Prev -> the main xref at the end
//! <document level objects>      catalog, page tree root, page 1's ancestors
//! <first page objects>          page 1, its content, fonts, images, ...
//! <everything else>
//! <main xref stream>            covers every object, no /Prev
//! startxref
//! %%EOF
//! ```
//!
//! Everything is written at the top level (no object streams) and every
//! offset that appears in a fixed-width field is padded, so the second pass
//! lands on the offsets the first one measured: the layout converges in two
//! passes, not in a loop.
//!
//! What is *not* here: the hint streams. /H is omitted, which a reader is
//! allowed to cope with; it just means a client cannot ask for "the byte ranges
//! of the objects of page 7" without walking the page tree itself.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write as _;

use pdf_render::pdf::file::{File, NoCache, NoLog};
use pdf_render::pdf::object::{ParseOptions, PlainRef, Resolve};
use pdf_render::pdf::primitive::Primitive;

/// Keys that are never followed while collecting a page's objects: /Parent
/// walks the page tree backwards, /Kids walks it forwards (and every sibling
/// page with it). A page's own contents need neither, and the ancestors of
/// page 1 are seeded explicitly.
const SKIP: &[&str] = &["Parent", "Kids"];
/// The catalog reaches into the outline, the name trees and the structure
/// tree, none of which page 1 needs. They belong to the rest of the file.
const SKIP_CATALOG: &[&str] = &[
    "Parent", "Outlines", "Names", "Dests", "StructTreeRoot", "Threads", "OpenAction",
    "AcroForm", "OCProperties", "Metadata", "Kids",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: lin <in.pdf> <out.pdf>");
        std::process::exit(2);
    }
    let (inp, outp) = (&args[1], &args[2]);

    let data = match std::fs::read(inp) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{inp}: {e}");
            std::process::exit(1);
        }
    };

    let file = match File::load_data(
        &data[..],
        b"",
        ParseOptions::strict(),
        NoCache,
        NoCache,
        NoLog,
    ) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{inp}: {e}");
            std::process::exit(1);
        }
    };

    // Every object the file has, resolved once. An id that does not resolve is
    // free or broken and is not written.
    let size = file.trailer.size.max(1) as u32;
    let mut prims: BTreeMap<u32, Primitive> = BTreeMap::new();
    for id in 1..size {
        if let Ok(p) = file.resolve(PlainRef { id: id as u64, gen: 0 }) {
            prims.insert(id, p);
        }
    }

    let catalog = file.trailer.root.get_ref().get_inner();
    let pages = file.num_pages();
    let page1 = match file.get_page(0) {
        Ok(p) => p.get_ref(),
        Err(e) => {
            eprintln!("page 1: {e}");
            std::process::exit(1);
        }
    };

    // The first-page part: what a reader needs to draw page 1, and nothing
    // else. Collected by walking the object graph from the page.
    let mut first: Vec<u32> = Vec::new();
    let mut seen: BTreeSet<u32> = BTreeSet::new();

    // Document level: the catalog, page 1's ancestors (its /Parent chain, which
    // SKIP keeps us from meeting on the way in), then page 1 itself.
    let mut seeds: Vec<(PlainRef, bool)> = vec![(catalog, true)];
    let mut cur_id = page1.id as u32;
    for _ in 0..32 {
        let parent = prims.get(&cur_id).and_then(|p| match p {
            Primitive::Dictionary(d) => d.get("Parent"),
            _ => None,
        });
        match parent {
            Some(Primitive::Reference(r)) => {
                seeds.push((*r, true));
                cur_id = r.id as u32;
            }
            _ => break,
        }
    }
    seeds.push((page1, true));

    let mut queue: VecDeque<(PlainRef, bool)> = VecDeque::new();
    for (r, deep) in seeds {
        queue.push_back((r, deep));
    }
    while let Some((r, deep)) = queue.pop_front() {
        if !seen.insert(r.id as u32) {
            continue;
        }
        let Some(p) = prims.get(&(r.id as u32)) else { continue };
        first.push(r.id as u32);
        if !deep {
            continue;
        }
        // A page met from somewhere else (a link's destination) is included,
        // but its contents are not page 1's business.
        if is_page(p) && r.id as u32 != page1.id as u32 {
            continue;
        }
        let skip = if r.id as u32 == catalog.id as u32 { SKIP_CATALOG } else { SKIP };
        let mut refs = Vec::new();
        push_refs(p, skip, &mut refs);
        for c in refs {
            queue.push_back((c, true));
        }
    }

    let rest: Vec<u32> = prims
        .keys()
        .copied()
        .filter(|id| !seen.contains(id))
        .collect();

    // New object numbers for the three dictionaries of our own.
    let lay = Layout {
        lin_id: size,
        fx_id: size + 1,
        mx_id: size + 2,
        size: size + 3,
        catalog,
        page1: page1.id as u32,
        pages,
        first,
        rest,
    };

    // Pass one: measure. Every field that depends on an offset is padded to a
    // fixed width, so the lengths here are the lengths of the finished file.
    let meas = render(&file, &prims, &lay, None);
    let end = render(&file, &prims, &lay, Some(&meas.pos));
    if end.pos != meas.pos {
        eprintln!("layout did not converge: offsets moved on the second pass");
        std::process::exit(1);
    }
    if let Err(e) = std::fs::write(outp, &end.bytes) {
        eprintln!("{outp}: {e}");
        std::process::exit(1);
    }

    println!(
        "{inp}: {} object(s), {} page(s)\n  page 1: {} object(s) inside the first {} byte(s)\n  rest: {} object(s) after that\n  out: {} byte(s) (in {})\n  -> {outp}",
        prims.len(),
        pages,
        lay.first.len() + 2,
        end.e,
        lay.rest.len(),
        end.bytes.len(),
        data.len(),
    );
}

fn is_page(p: &Primitive) -> bool {
    match p {
        Primitive::Dictionary(d) => match d.get("Type") {
            Some(Primitive::Name(n)) => n.as_str() == "Page",
            _ => false,
        },
        _ => false,
    }
}

fn push_refs(p: &Primitive, skip: &[&str], out: &mut Vec<PlainRef>) {
    match p {
        Primitive::Reference(r) => out.push(*r),
        Primitive::Array(a) => {
            for v in a {
                push_refs(v, skip, out);
            }
        }
        Primitive::Dictionary(d) => {
            for (k, v) in d.iter() {
                if skip.iter().any(|s| k.as_str() == *s) {
                    continue;
                }
                push_refs(v, skip, out);
            }
        }
        Primitive::Stream(s) => {
            for (k, v) in s.info.iter() {
                // /Length is written by the copier, not followed.
                if k.as_str() == "Length" || skip.iter().any(|s| k.as_str() == *s) {
                    continue;
                }
                push_refs(v, skip, out);
            }
        }
        _ => {}
    }
}

struct Layout {
    lin_id: u32,
    fx_id: u32,
    mx_id: u32,
    size: u32,
    catalog: PlainRef,
    page1: u32,
    pages: u32,
    first: Vec<u32>,
    rest: Vec<u32>,
}

struct Rendered {
    bytes: Vec<u8>,
    /// Where each object starts, plus two markers: `TOTAL` for the file length
    /// and `END_FIRST` for the end of the first-page part.
    pos: BTreeMap<u32, u32>,
    /// End of the first-page part: the /E of the linearization dictionary.
    e: u32,
}

const TOTAL: u32 = u32::MAX;
const END_FIRST: u32 = u32::MAX - 1;

fn render<F: Resolve>(
    file: &F,
    prims: &BTreeMap<u32, Primitive>,
    lay: &Layout,
    off: Option<&BTreeMap<u32, u32>>,
) -> Rendered {
    let mut out: Vec<u8> = Vec::with_capacity(1 << 20);
    out.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");

    let mut pos: BTreeMap<u32, u32> = BTreeMap::new();
    let at = |id: u32| off.and_then(|m| m.get(&id).copied()).unwrap_or(0);

    // 2. The linearization dictionary, /L /T /E padded: the same length before
    //    and after the offsets are known.
    pos.insert(lay.lin_id, out.len() as u32);
    let _ = write!(
        out,
        "{} 0 obj\n<< /Linearized 1 /L {:>10} /O {} /E {:>10} /N {} /T {:>10} >>\nendobj\n",
        lay.lin_id,
        at(TOTAL),
        lay.page1,
        at(END_FIRST),
        lay.pages,
        at(lay.fx_id),
    );

    // 3. The first-page cross-reference section: the first-page part only, and
    //    its /Prev is the main section at the end of the file — a reader that
    //    started at byte 0 finds the rest of the index from here.
    let mut first_ids: Vec<u32> = Vec::with_capacity(lay.first.len() + 1);
    first_ids.push(lay.lin_id);
    first_ids.extend(lay.first.iter().copied());
    first_ids.sort_unstable();
    pos.insert(lay.fx_id, out.len() as u32);
    out.extend_from_slice(&xref_obj(
        lay.fx_id,
        lay.catalog,
        lay.size,
        &first_ids,
        &at,
        Some(at(lay.mx_id)),
    ));

    // 4, 5. The first-page part.
    for &id in lay.first.iter() {
        pos.insert(id, out.len() as u32);
        if let Some(p) = prims.get(&id) {
            write_obj(&mut out, id, p, file);
        }
    }
    let e_real = out.len() as u32;

    // 6, 7, 8. The rest.
    for &id in lay.rest.iter() {
        pos.insert(id, out.len() as u32);
        if let Some(p) = prims.get(&id) {
            write_obj(&mut out, id, p, file);
        }
    }

    // 9. The main cross-reference section: every object, no /Prev.
    let mut all: Vec<u32> = pos
        .keys()
        .copied()
        .filter(|&id| id < lay.size)
        .collect();
    all.sort_unstable();
    all.dedup();
    pos.insert(lay.mx_id, out.len() as u32);
    out.extend_from_slice(&xref_obj(lay.mx_id, lay.catalog, lay.size, &all, &at, None));

    let xref_pos = pos[&lay.mx_id];
    let _ = write!(out, "startxref\n{}\n%%EOF\n", xref_pos);
    pos.insert(TOTAL, out.len() as u32);
    pos.insert(END_FIRST, e_real);

    Rendered { bytes: out, pos, e: e_real }
}

/// One object, top level: its dictionary and, for a stream, the bytes exactly
/// as they were stored (the filters are the source's).
fn write_obj<F: Resolve>(out: &mut Vec<u8>, id: u32, p: &Primitive, file: &F) {
    match p {
        Primitive::Stream(s) => {
            let data = match s.raw_data(file) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("object {id}: cannot read the stream data: {e}");
                    return;
                }
            };
            let mut info = s.info.clone();
            info.insert("Length", Primitive::Integer(data.len() as i64));
            let _ = write!(out, "{id} 0 obj\n");
            let _ = info.serialize(out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&data);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        }
        p => {
            let _ = write!(out, "{id} 0 obj\n");
            let _ = p.serialize(out);
            if !out.last().map(|b| b.is_ascii_whitespace()).unwrap_or(false) {
                out.push(b'\n');
            }
            out.extend_from_slice(b"endobj\n");
        }
    }
}

/// A cross-reference stream (/W [1 4 2]) for `ids`, with /Prev when given.
fn xref_obj(
    id: u32,
    root: PlainRef,
    size: u32,
    ids: &[u32],
    at: &impl Fn(u32) -> u32,
    prev: Option<u32>,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(ids.len() * 7);
    let mut index: Vec<String> = Vec::new();
    let mut run_start: Option<u32> = None;
    let mut prev_id = 0u32;
    for &obj in ids {
        if run_start.is_none() || obj != prev_id + 1 {
            if let Some(s) = run_start {
                index.push(s.to_string());
                index.push((prev_id - s + 1).to_string());
            }
            run_start = Some(obj);
        }
        prev_id = obj;
        data.push(1);
        data.extend_from_slice(&at(obj).to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
    }
    if let Some(s) = run_start {
        index.push(s.to_string());
        index.push((prev_id - s + 1).to_string());
    }

    let mut out = Vec::with_capacity(data.len() + 256);
    let _ = write!(
        out,
        "{id} 0 obj\n<< /Type /XRef /Size {size} /Root {} {} R /W [1 4 2] /Index [{}] /Length {}",
        root.id,
        root.gen,
        index.join(" "),
        data.len()
    );
    if let Some(p) = prev {
        let _ = write!(out, " /Prev {:>10}", p);
    }
    out.extend_from_slice(b" >>\nstream\n");
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out
}
