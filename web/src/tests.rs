//! The two things that can go wrong in a viewer that fetches by range: it
//! never converges, or it renders something else than the whole file would.
//!
//! Both are checked against a real document, with a network in front of it
//! that only answers what it is asked for — no shortcuts, the same code path
//! the browser takes.

use std::path::PathBuf;
use std::sync::Arc;

use super::*;

const OPEN: u32 = 0xFFFF_FFFF;

/// A document and a server that hands out ranges of it.
struct Net {
    bytes: Vec<u8>,
    /// Round trips, and bytes that crossed the wire.
    requests: usize,
    fetched: usize,
}

impl Net {
    fn new(bytes: Vec<u8>) -> Net {
        Net { bytes, requests: 0, fetched: 0 }
    }

    /// Ask, fetch, supply, and ask again — what the page does in JavaScript.
    fn serve(&mut self, doc: &mut Doc, page: u32) -> Result<(), String> {
        for round in 1..=32 {
            match doc.prepare(page) {
                Ready::Need(ranges) => {
                    if ranges.is_empty() {
                        return Err("a request with no ranges".into());
                    }
                    eprintln!(
                        "round {round}: {} range(s), {} bytes",
                        ranges.len(),
                        ranges.iter().map(|r| r.len()).sum::<usize>()
                    );
                    self.requests += 1;
                    for r in ranges {
                        if r.end > self.bytes.len() {
                            return Err(format!("asked for {r:?} of {} bytes", self.bytes.len()));
                        }
                        self.fetched += r.len();
                        doc.supply(r.start as u32, &self.bytes[r]);
                    }
                }
                Ready::Failed => {
                    return Err(doc.error().unwrap_or("failed without a reason").into())
                }
                Ready::Page | Ready::Opened(_) | Ready::Outline => return Ok(()),
            }
        }
        Err("did not converge in 8 round trips".into())
    }
}

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The standard fonts as the bundle the browser would fetch.
///
/// Built from the directory rather than read from a prebuilt file, so the test
/// covers `write_plain` and `parse_bundle` as well as the render.
fn font_bundle() -> Option<Vec<u8>> {
    let dir = repo("../standard_fonts");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "README.md" {
            continue;
        }
        let data: Arc<[u8]> = std::fs::read(entry.path()).ok()?.into();
        files.push((name, data));
    }
    (!files.is_empty()).then(|| MemDir::new(files).write_plain())
}

fn logo() -> Vec<u8> {
    std::fs::read(repo("../wasm/logo.pdf")).expect("wasm/logo.pdf")
}

/// The page, rendered from the whole file at once: what a ranged render has to
/// match.
fn whole_file(bytes: &[u8], page: u32) -> String {
    let mut doc = Doc::new(bytes.len() as u32);
    if let Some(bundle) = font_bundle() {
        doc.set_fonts(&bundle).unwrap();
    }
    doc.supply(0, bytes);
    match doc.prepare(page) {
        Ready::Page => doc.take_svg(page).expect("rendered page"),
        Ready::Failed => panic!("whole file failed: {:?}", doc.error()),
        _ => panic!("whole file still wants bytes"),
    }
}

fn ranged(bytes: &[u8], page: u32) -> (String, Net) {
    let mut doc = Doc::new(bytes.len() as u32);
    if let Some(bundle) = font_bundle() {
        doc.set_fonts(&bundle).unwrap();
    }
    let mut net = Net::new(bytes.to_vec());
    net.serve(&mut doc, page).unwrap();
    let svg = doc.take_svg(page).expect("rendered page");
    (svg, net)
}

#[test]
fn the_document_opens_and_says_how_many_pages_it_has() {
    let bytes = logo();
    let mut doc = Doc::new(bytes.len() as u32);
    let mut net = Net::new(bytes.clone());
    assert_eq!(doc.num_pages(), 0, "nothing is known before a byte arrives");

    net.serve(&mut doc, OPEN).unwrap();
    let whole = {
        let mut doc = Doc::new(bytes.len() as u32);
        doc.supply(0, &bytes);
        match doc.prepare(OPEN) {
            Ready::Opened(n) => n,
            other => panic!("unexpected {other:?}", other = match other {
                Ready::Failed => format!("failed: {:?}", doc.error()),
                _ => "not opened".to_string(),
            }),
        }
    };
    assert!(whole > 0, "logo.pdf has pages");
    assert_eq!(doc.num_pages(), whole);
    // Opening costs the tail of the file — and for a 3 kB file the widening
    // around that tail is the whole thing, which is the right trade.
    assert!(net.fetched <= bytes.len(), "{} of {} bytes", net.fetched, bytes.len());
    assert!(net.requests <= 2, "{} round trips to open", net.requests);
}

#[test]
fn a_page_fetched_by_range_is_the_page_of_the_whole_file() {
    let bytes = logo();
    let (ranged_svg, net) = ranged(&bytes, 0);
    assert_eq!(ranged_svg, whole_file(&bytes, 0));
    // Three round trips is the budget: the tail, what the page tree and fonts
    // need, and possibly one object that mention of which was indirect.
    assert!(net.requests <= 3, "{} round trips", net.requests);
    assert!(net.fetched <= bytes.len());
    // It is a document, and it has content.
    assert!(ranged_svg.starts_with("<svg "), "{}", &ranged_svg[..40.min(ranged_svg.len())]);
    assert!(ranged_svg.ends_with("</svg>"));
    assert!(ranged_svg.contains("<path"), "the page drew nothing");
}

/// The numbers a real document gives, rather than the ones a 3 KiB logo can.
///
/// Skipped when the sample is not there: this is a property of the mechanism,
/// and the small-document test above already covers correctness.
#[test]
fn a_page_of_a_large_document_costs_a_fraction_of_it() {
    let path = std::env::var_os("PDF_WEB_SAMPLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/hematite/ia/WG73-WriterGuide.pdf"));
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipped: no sample at {}", path.display());
        return;
    };
    let (svg, net) = ranged(&bytes, 0);
    let share = net.fetched as f64 / bytes.len() as f64;
    let pages = {
        let mut doc = Doc::new(bytes.len() as u32);
        let mut net = Net::new(bytes.clone());
        net.serve(&mut doc, OPEN).unwrap();
        doc.num_pages()
    };
    let mut report = format!(
        "{}: {} bytes, {} page(s); page 1: {} bytes in {} round trip(s) ({:.2} %), svg {} bytes\n",
        path.display(),
        bytes.len(),
        pages,
        net.fetched,
        net.requests,
        share * 100.0,
        svg.len(),
    );
    // Page 3 is text; the cover of the guide is a full-page image and says
    // nothing about the vector path.
    let (text_svg, text_net) = ranged(&bytes, 2);
    report.push_str(&format!(
        "page 3: {} bytes in {} round trip(s) ({:.2} %), svg {} bytes, {} path(s)\n",
        text_net.fetched,
        text_net.requests,
        text_net.fetched as f64 / bytes.len() as f64 * 100.0,
        text_svg.len(),
        text_svg.matches("<path").count(),
    ));
    eprintln!("{report}");
    // The numbers of a run, for the report to whoever is reading it.
    std::fs::write(std::env::temp_dir().join("pdf_web_numbers.txt"), &report).ok();
    assert_eq!(text_svg, whole_file(&bytes, 2));
    assert!(text_svg.contains("<path"), "page 3 has text");
    // A page that needed most of a 25 MB document would mean the mechanism is
    // not doing anything.
    let share = text_net.fetched as f64 / bytes.len() as f64;
    assert!(share < 0.25, "page 3 cost {:.1} % of the file", share * 100.0);
}

#[test]
fn a_page_that_is_asked_for_twice_still_renders() {
    let bytes = logo();
    let (first, _) = ranged(&bytes, 0);
    let (second, _) = ranged(&bytes, 0);
    assert_eq!(first, second, "the same document rendered twice");
}

#[test]
fn asking_again_without_fetching_does_not_loop() {
    // A host that loses a response must not be able to spin the module: the
    // second identical request is refused with an error instead.
    let bytes = logo();
    let mut doc = Doc::new(bytes.len() as u32);
    let mut net = Net::new(bytes);
    // One round of genuine fetching, then nothing.
    let first = match doc.prepare(0) {
        Ready::Need(ranges) => ranges,
        other => panic!("expected to want bytes, got {}", describe(&other)),
    };
    for r in first {
        doc.supply(r.start as u32, &net.bytes[r]);
    }
    let _ = &mut net;
    // Whatever happens next is either a render or a further request; what must
    // not happen is the module asking for the same bytes for ever.
    let mut rounds = 0;
    loop {
        match doc.prepare(0) {
            Ready::Need(ranges) => {
                rounds += 1;
                assert!(rounds < 4, "asked for bytes {rounds} times in a row");
                if ranges.is_empty() {
                    break;
                }
                // Supply nothing this time.
            }
            Ready::Page | Ready::Opened(_) | Ready::Outline | Ready::Failed => break,
        }
    }
}

#[test]
fn a_supplied_range_beyond_the_end_is_ignored() {
    let mut doc = Doc::new(100);
    doc.supply(90, &[1u8; 50]);
    assert_eq!(doc.held(), 10);
    doc.supply(200, &[1u8; 10]);
    assert_eq!(doc.held(), 10);
}

fn describe(ready: &Ready) -> String {
    match ready {
        Ready::Need(r) => format!("need {} range(s)", r.len()),
        Ready::Page => "page".into(),
        Ready::Opened(n) => format!("opened, {n} page(s)"),
        Ready::Outline => "outline".into(),
        Ready::Failed => "failed".into(),
    }
}

// --- the outline ---------------------------------------------------------

/// The JSON back inside the test. The outline is a text format, so the test
/// reads it as one rather than trusting a substring.
fn entries(json: &str) -> Vec<(u32, Option<u32>, String)> {
    let b = json.as_bytes();
    let digit = |i: &mut usize| {
        let start = *i;
        while b[*i].is_ascii_digit() {
            *i += 1;
        }
        json[start..*i].parse::<u32>().unwrap()
    };
    assert!(b.starts_with(b"["), "{json}");
    let mut i = 1;
    let mut out = Vec::new();
    while b[i] == b'[' {
        i += 1;
        let depth = digit(&mut i);
        assert_eq!(b[i], b',');
        i += 1;
        let page = if b[i..].starts_with(b"null") {
            i += 4;
            None
        } else {
            Some(digit(&mut i))
        };
        assert_eq!(b[i], b',');
        assert_eq!(b[i + 1], b'"');
        i += 2;
        let mut title = String::new();
        while b[i] != b'"' {
            if b[i] == b'\\' {
                i += 1;
                match b[i] {
                    b'"' => title.push('"'),
                    b'\\' => title.push('\\'),
                    b'n' => title.push('\n'),
                    b'r' => title.push('\r'),
                    b't' => title.push('\t'),
                    b'u' => {
                        let code = u32::from_str_radix(&json[i + 1..i + 5], 16).unwrap();
                        title.push(char::from_u32(code).unwrap());
                        i += 4;
                    }
                    other => panic!("escape \\{}", other as char),
                }
                i += 1;
            } else {
                let ch = json[i..].chars().next().unwrap();
                title.push(ch);
                i += ch.len_utf8();
            }
        }
        i += 1;
        assert_eq!(b[i], b']');
        i += 1;
        out.push((depth, page, title));
        if b[i] == b',' {
            i += 1;
        }
    }
    assert_eq!(b[i], b']');
    out
}

#[test]
fn a_document_without_an_outline_has_an_empty_one() {
    let bytes = logo();
    let mut doc = Doc::new(bytes.len() as u32);
    doc.supply(0, &bytes);
    match doc.prepare(OUTLINE_ONLY) {
        Ready::Outline => {}
        other => panic!("expected an outline, got {}", describe(&other)),
    }
    let json = doc.take_outline().expect("the outline the pass built");
    assert!(entries(&json).is_empty(), "{json}");
    // And it is not handed out twice.
    assert!(doc.take_outline().is_none());
}

/// A real table of contents, read by range: every row that names a page names
/// one this document has, and the nesting starts at the top.
#[test]
fn the_outline_of_a_large_document_names_pages_it_has() {
    let path = std::env::var_os("PDF_WEB_SAMPLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/hematite/ia/WG73-WriterGuide.pdf"));
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipped: no sample at {}", path.display());
        return;
    };
    let (rows, pages, net) = {
        let mut doc = Doc::new(bytes.len() as u32);
        if let Some(bundle) = font_bundle() {
            doc.set_fonts(&bundle).unwrap();
        }
        let mut net = Net::new(bytes.clone());
        net.serve(&mut doc, OPEN).unwrap();
        net.serve(&mut doc, OUTLINE_ONLY).unwrap();
        let json = doc.take_outline().expect("an outline");
        (entries(&json), doc.num_pages(), net)
    };
    eprintln!(
        "outline: {} row(s), {} of {} bytes, {} round trips",
        rows.len(),
        net.fetched,
        bytes.len(),
        net.requests
    );
    assert!(!rows.is_empty(), "the guide has a table of contents");
    assert_eq!(rows[0].0, 0, "the first row is a top-level one");
    assert!(rows.iter().any(|row| row.1.is_some()), "no row points at a page");
    assert!(rows.iter().any(|row| row.0 > 0), "the outline has no nesting");
    for (depth, page, title) in &rows {
        assert!(*depth as usize <= 12, "{title}: nested {depth} deep");
        if let Some(page) = page {
            assert!(*page < pages, "{title}: page {page} of {pages}");
        }
    }
    // The table of contents is a fraction of the document; it must not be
    // what makes the first page turn expensive.
    assert!(net.fetched < bytes.len() / 2);
}

/// The same outline from a ranged read and from the whole file at once.
#[test]
fn the_outline_of_a_ranged_read_is_the_outline_of_the_whole_file() {
    let path = std::env::var_os("PDF_WEB_SAMPLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/hematite/ia/WG73-WriterGuide.pdf"));
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipped: no sample at {}", path.display());
        return;
    };
    let mut whole = Doc::new(bytes.len() as u32);
    if let Some(bundle) = font_bundle() {
        whole.set_fonts(&bundle).unwrap();
    }
    whole.supply(0, &bytes);
    assert!(matches!(whole.prepare(OUTLINE_ONLY), Ready::Outline));
    let expected = whole.take_outline().unwrap();

    let (got, _) = {
        let mut doc = Doc::new(bytes.len() as u32);
        if let Some(bundle) = font_bundle() {
            doc.set_fonts(&bundle).unwrap();
        }
        let mut net = Net::new(bytes.clone());
        net.serve(&mut doc, OUTLINE_ONLY).unwrap();
        (doc.take_outline().expect("an outline"), net)
    };
    assert_eq!(got, expected, "the outline depends on how the bytes arrived");
}
