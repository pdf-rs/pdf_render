//! What the data section is made of, without a toolchain.
//!
//! Rust puts almost every string literal, every `include_bytes!` blob and every
//! `static` table into the data section; the code section holds instructions.
//! So "574 kB of data" is only interesting once you know whether it is prose
//! (panic messages, font and table names) or tables.
//!
//!   wasm_report ../www/pdf_web.wasm          sections, imports, gzip
//!   wasm_strings ../www/pdf_web.wasm         the strings inside, longest first
//!   wasm_strings ../www/pdf_web.wasm foo     only runs containing `foo`

use std::collections::BTreeMap;
use std::process::ExitCode;

fn leb(r: &mut Reader) -> Option<u64> {
    let mut out = 0u64;
    let mut shift = 0;
    loop {
        let b = r.u8()?;
        out |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(out);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }
}

/// Every section body in the file, in order: (id, body).
fn sections(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut r = Reader::new(&data[8..]); // skip magic + version
    while r.at < r.b.len() {
        let Some(id) = r.u8() else { break };
        let Some(len) = leb(&mut r) else { break };
        let Some(body) = r.bytes(len as usize) else {
            break;
        };
        out.push((id, body));
    }
    out
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: wasm_strings <file.wasm> [substring]");
        return ExitCode::FAILURE;
    };
    let filter = args.next();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if data.len() < 8 || &data[..4] != b"\0asm" {
        eprintln!("{path}: not a wasm module");
        return ExitCode::FAILURE;
    }
    println!("file:  {path}");
    println!("bytes: {}", data.len());

    let mut total_data = 0usize;
    let mut printable = 0usize;
    let mut runs: Vec<(usize, String)> = Vec::new();
    let mut by_prefix: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for (id, body) in sections(&data) {
        if id != 11 {
            continue; // only the data section
        }
        total_data += body.len();
        // Pubnix: don't parse the segments (mode, offset, length); scan the raw
        // bytes. A run through the segment headers occasionally produces a short
        // false string, which costs nothing at this resolution.
        let mut start = None;
        for (i, &b) in body.iter().enumerate() {
            let printable_char = (0x20..=0x7e).contains(&b);
            match (printable_char, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    let run = &body[s..i];
                    printable += run.len();
                    if run.len() >= 8 {
                        let s = String::from_utf8_lossy(run).into_owned();
                        if filter.as_deref().is_none_or(|f| s.contains(f)) {
                            // Group by the first identifier-looking word, which
                            // is usually the crate or table the string is for.
                            let key: String = s
                                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                                .find(|w| w.len() >= 3)
                                .unwrap_or("?")
                                .chars()
                                .take(24)
                                .collect();
                            let e = by_prefix.entry(key).or_default();
                            e.0 += 1;
                            e.1 += run.len();
                        }
                        runs.push((run.len(), s));
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }

    println!("data:  {total_data} bytes, {printable} of them printable ASCII");
    runs.sort_by(|a, b| b.0.cmp(&a.0));
    println!("strings: {} of 8+ bytes", runs.len());
    println!("\nlongest 25:");
    for (n, s) in runs.iter().take(25) {
        let s: String = s.chars().take(96).collect();
        println!("  {n:>6}  {s}");
    }
    println!("\nby first word (count, bytes):");
    let mut groups: Vec<_> = by_prefix.into_iter().collect();
    groups.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (k, (count, bytes)) in groups.iter().take(30) {
        println!("  {bytes:>7}  {count:>5}×  {k}");
    }
    ExitCode::SUCCESS
}
