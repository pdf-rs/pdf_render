//! The document's own table of contents, for the sidebar.
//!
//! The outline is a chain of dictionaries: `/Outlines /First` is the first
//! row, each row's `/Next` is the row below it, its `/First` is the first row
//! of its children, and so on. A row is a title and a place to go — either
//! `/Dest` (a destination) or `/A` (an action, of which only `/GoTo` matters
//! here). Destinations come in three shapes: an array whose first item is the
//! page, a name into the document's named destinations, or a dictionary with
//! `/D` around one of those.
//!
//! Walked over raw primitives rather than through `OutlineItem`, on purpose: a
//! row whose `/A` is an action this crate cannot model fails to parse as a
//! whole, and the reader would lose a heading — with its children — over a
//! field that has nothing to do with where it points. Here a row that cannot
//! be read is one row without a place to go.
//!
//! The walk is bounded three ways: entries (a document with 4000 headings is
//! not a table of contents), depth, and a set of dictionaries already entered
//! — `/Next` and `/First` may point back, and a chain that does is endless.
//!
//! Output is JSON: `[[depth, page or null, "title"], …]`, `page` 0-based, the
//! same numbering the viewer's `?page=` uses. A row with `null` has no
//! destination in this document (an external file, a JavaScript action) and is
//! shown, but not clickable — dropping it would silently empty a chapter.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use pdf_render::pdf::object::{Catalog, Dest, PagesNode, PageTree, PlainRef, Resolve};
use pdf_render::pdf::primitive::{PdfString, Primitive};

/// Entries beyond this are not a table of contents. The smallest real ones
/// (a 200-page report) are a few hundred rows.
const MAX_ENTRIES: usize = 4000;

/// Levels below the top. Twelve nested levels is already a document with a
/// problem, not an outline.
const MAX_DEPTH: usize = 12;

/// Titles longer than this are cut: a heading is a line in a sidebar, not a
/// paragraph, and a pathological string should not become the sidebar's width.
const MAX_TITLE: usize = 400;

/// Pages beyond this are not a document a viewer will scroll through, and the
/// walk below would be the only thing standing between a hostile `/Kids` tree
/// and the tab.
const MAX_PAGES: usize = 100_000;

/// The page refs in order: one entry per leaf of the page tree, `None` where a
/// node will not parse (the entry stays, so the pages after it keep their
/// numbers).
fn page_refs(r: &impl Resolve, tree: &PageTree, out: &mut Vec<Option<PlainRef>>, depth: usize) {
    if depth > 64 || out.len() >= MAX_PAGES {
        return;
    }
    for kid in &tree.kids {
        if out.len() >= MAX_PAGES {
            return;
        }
        let pref = kid.get_inner();
        match r.get(*kid) {
            Ok(node) => match &*node {
                PagesNode::Tree(tree) => page_refs(r, tree, out, depth + 1),
                PagesNode::Leaf(_) => out.push(Some(pref)),
            },
            Err(_) => out.push(None),
        }
    }
}

struct Item {
    depth: usize,
    /// 0-based page, or `None` when the row points at something else.
    page: Option<usize>,
    title: String,
}

/// Build the outline of `catalog` as JSON.
///
/// Page numbers are the viewer's: the leaves of `/Pages`, counted in order,
/// which is what `File::get_page` numbers. A page that will not parse is left
/// out of the map and simply cannot be a target.
pub fn build(r: &impl Resolve, catalog: &Catalog) -> String {
    // One walk of the page tree, not one `get_page` per page: the page index
    // is the position of a leaf ref, and a tree walk visits every node once
    // where `get_page(n)` for every n walks it from the root each time.
    let mut pages: Vec<Option<PlainRef>> = Vec::new();
    page_refs(r, &catalog.pages, &mut pages, 0);

    let mut index: HashMap<PlainRef, usize> = HashMap::with_capacity(pages.len());
    for (i, page) in pages.iter().enumerate() {
        if let Some(page) = page {
            index.insert(*page, i);
        }
    }

    let mut named = named_dests(r, catalog, &index);

    // `/Dests` in the catalog is the older shape of the same idea: a plain
    // dictionary from name to destination array (PDF 1.1, still written).
    if let Some(dests) = &catalog.dests {
        for (name, value) in dests.iter() {
            if named.len() >= MAX_ENTRIES {
                break;
            }
            if let Some(page) = dest_page(r, &index, &named, value, 0) {
                named.insert(name.as_str().to_string(), page);
            }
        }
    }

    let mut walk = Walk { r, pages: &index, named: &named, items: Vec::new(), seen: HashSet::new() };
    if let Some(outlines) = &catalog.outlines {
        if let Some(first) = outlines.first {
            walk.level(first.get_inner(), 0);
        }
    }

    let mut out = String::with_capacity(2 + walk.items.len() * 56);
    out.push('[');
    for (i, item) in walk.items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "[{},", item.depth);
        match item.page {
            Some(page) => {
                let _ = write!(out, "{page}");
            }
            None => out.push_str("null"),
        }
        out.push(',');
        quote(&mut out, &item.title);
        out.push(']');
    }
    out.push(']');
    out
}

/// `name -> page` from the `/Names /Dests` name tree.
fn named_dests(
    r: &impl Resolve,
    catalog: &Catalog,
    pages: &HashMap<PlainRef, usize>,
) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let Some(names) = &catalog.names else { return out };
    let Some(dests) = &names.dests else { return out };
    // A name tree that will not walk costs the whole set, not the outline:
    // rows that used a name then simply have no destination.
    let _ = dests.walk(r, &mut |key: &PdfString, value: &Option<Dest>| {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        if let Some(Dest { page: Some(page), .. }) = value {
            if let Some(&i) = pages.get(&page.get_inner()) {
                out.insert(key.to_string_lossy(), i);
            }
        }
    });
    out
}

struct Walk<'a, R> {
    r: &'a R,
    pages: &'a HashMap<PlainRef, usize>,
    named: &'a HashMap<String, usize>,
    items: Vec<Item>,
    /// Dictionaries already entered: `/Next` and `/First` may form a cycle,
    /// and a chain that does would otherwise never end.
    seen: HashSet<PlainRef>,
}

impl<R: Resolve> Walk<'_, R> {
    /// One level of the outline: `pref` is its first row, `/Next` its next.
    fn level(&mut self, mut pref: PlainRef, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        while self.items.len() < MAX_ENTRIES {
            if !self.seen.insert(pref) {
                return;
            }
            let Ok(Primitive::Dictionary(dict)) = self.r.resolve(pref) else {
                return;
            };
            let title = dict
                .get("Title")
                .and_then(|t| t.as_string().ok())
                .map(|s| text(s.data.as_slice()))
                .unwrap_or_default();
            let page = dict
                .get("Dest")
                .and_then(|d| dest_page(self.r, self.pages, self.named, d, 0))
                .or_else(|| {
                    dict.get("A")
                        .and_then(|a| action_page(self.r, self.pages, self.named, a))
                });
            self.items.push(Item { depth, page, title });

            if let Some(Primitive::Reference(first)) = dict.get("First") {
                self.level(*first, depth + 1);
            }
            match dict.get("Next") {
                Some(Primitive::Reference(next)) => pref = *next,
                _ => return,
            }
        }
    }
}

/// The page a destination points at.
fn dest_page(
    r: &impl Resolve,
    pages: &HashMap<PlainRef, usize>,
    named: &HashMap<String, usize>,
    p: &Primitive,
    depth: usize,
) -> Option<usize> {
    if depth > 8 {
        return None;
    }
    match p {
        // `/Dest (chapter-three)` — a name into the name tree. Some writers
        // use a bare name object for the same thing.
        Primitive::String(name) => named.get(&name.to_string_lossy()).copied(),
        Primitive::Name(name) => named.get(name.as_str()).copied(),
        // `[page /XYZ left top zoom]` — the page is the first item.
        Primitive::Array(a) => target(a.first()?, pages),
        // `<< /D [page /Fit] >>`.
        Primitive::Dictionary(d) => dest_page(r, pages, named, d.get("D")?, depth + 1),
        // An indirect destination, or one that names another.
        Primitive::Reference(reference) => {
            let resolved = r.resolve(*reference).ok()?;
            dest_page(r, pages, named, &resolved, depth + 1)
        }
        // A bare page number is not what the specification says, but it is
        // what a few generators write.
        Primitive::Integer(n) => usize::try_from(*n).ok(),
        _ => None,
    }
}

fn target(first: &Primitive, pages: &HashMap<PlainRef, usize>) -> Option<usize> {
    match first {
        Primitive::Reference(page) => pages.get(page).copied(),
        Primitive::Integer(n) => usize::try_from(*n).ok(),
        _ => None,
    }
}

/// The page a `/GoTo` action points at; every other action kind is not a place
/// in this document.
fn action_page(
    r: &impl Resolve,
    pages: &HashMap<PlainRef, usize>,
    named: &HashMap<String, usize>,
    p: &Primitive,
) -> Option<usize> {
    let owned;
    let dict = match p {
        Primitive::Dictionary(dict) => dict,
        Primitive::Reference(reference) => {
            owned = r.resolve(*reference).ok()?.into_dictionary().ok()?;
            &owned
        }
        _ => return None,
    };
    if dict.get("S")?.as_name().ok()? != "GoTo" {
        return None;
    }
    dest_page(r, pages, named, dict.get("D")?, 0)
}

/// A title as text.
///
/// A PDF string is one of three things: UTF-16BE with a BOM, UTF-8 with a BOM,
/// or PDFDocEncoding — Latin-1 everywhere except 0x18–0x1F and 0x80–0xA0,
/// where the punctuation a book's headings are full of lives: the curly quotes,
/// the dashes, the ellipsis. Reading those bytes as if they were UTF-8 is what
/// turns "Editors' Introduction" into "Editors\u{fffd} Introduction", so the
/// bytes are decoded as what the specification says they are, and only a string
/// that is valid UTF-8 throughout is taken at its word.
fn text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xfe, 0xff]) {
        return pdf_render::pdf::font::utf16be_to_string_lossy(&bytes[2..]);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| pdfdoc_char(b)).collect(),
    }
}

/// PDFDocEncoding, Annex D of PDF 32000-1: the codes that are not Latin-1.
fn pdfdoc_char(b: u8) -> char {
    match b {
        0x18 => '\u{02d8}',
        0x19 => '\u{02c7}',
        0x1a => '\u{02c6}',
        0x1b => '\u{02d9}',
        0x1c => '\u{02dd}',
        0x1d => '\u{02db}',
        0x1e => '\u{02da}',
        0x1f => '\u{02dc}',
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{0192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203a}',
        0x8a => '\u{2212}',
        0x8b => '\u{2030}',
        0x8c => '\u{201e}',
        0x8d => '\u{201c}',
        0x8e => '\u{201d}',
        0x8f => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201a}',
        0x92 => '\u{2122}',
        0x93 => '\u{fb01}',
        0x94 => '\u{fb02}',
        0x95 => '\u{0141}',
        0x96 => '\u{0152}',
        0x97 => '\u{0160}',
        0x98 => '\u{0178}',
        0x99 => '\u{017d}',
        0x9a => '\u{0131}',
        0x9b => '\u{0142}',
        0x9c => '\u{0153}',
        0x9d => '\u{0161}',
        0x9e => '\u{017e}',
        // 0x9F is not assigned; nothing sensible to show, so say so.
        0x9f => '\u{fffd}',
        0xa0 => '\u{20ac}',
        // 0x00–0x17 and 0x20–0xFF are Latin-1.
        b => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_decoded_as_what_it_is() {
        // UTF-16BE with a BOM: "Editors’ Introduction".
        let mut utf16 = vec![0xfe, 0xff];
        for unit in "Editors’ Introduction".encode_utf16() {
            utf16.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(text(&utf16), "Editors’ Introduction");
        // PDFDocEncoding: the same apostrophe is one byte.
        assert_eq!(text(b"Editors\x90 Introduction"), "Editors’ Introduction");
        // Latin-1, a dash, an ellipsis.
        assert_eq!(text(b"caf\xe9 \x84 \x83"), "café — …");
        // UTF-8 with and without a BOM.
        assert_eq!(text("café ✓".as_bytes()), "café ✓");
        assert_eq!(text("\u{feff}café".as_bytes()), "café");
    }

    #[test]
    fn a_title_with_a_quote_in_it_is_still_json() {
        let mut out = String::new();
        quote(&mut out, "a \"b\" \\c\n");
        assert_eq!(out, r#""a \"b\" \\c\n""#);
        let long = "x".repeat(MAX_TITLE + 10);
        let mut out = String::new();
        quote(&mut out, &long);
        assert!(out.ends_with("…\""), "{}", &out[out.len() - 8..]);
        assert_eq!(out.chars().count(), MAX_TITLE + 3);
    }
}fn quote(out: &mut String, s: &str) {
    out.push('"');
    for (n, ch) in s.chars().enumerate() {
        if n >= MAX_TITLE {
            out.push('…');
            break;
        }
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // A page of a PDF may carry text that is not a character at all
            // (a lone surrogate comes back as U+FFFD, which is fine here).
            c => out.push(c),
        }
    }
    out.push('"');
}
