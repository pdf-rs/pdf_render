//! Break a wasm module down by crate, from the linker's map.
//!
//! `wasm_report` says "code 1.49 MB, data 574 kB"; that is a fact about the
//! file, not about the code. The map `wasm-ld --Map` writes attributes every
//! symbol to the object file it came from, and the object file's name carries
//! the crate — so summing by object gives "how much of the module is pdf
//! parsing, how much is the SVG backend, how much is std".
//!
//!   cargo build --release -p pdf_web          (with -C link-arg=--Map=…)
//!   wasm_map /path/to/pdf_web.map
//!
//! `In` looks like
//!   /…/deps/libpdf_encoding-b414de41.rlib(pdf_encoding-b414de41.pdf_encoding.36aa12be-cgu.0.rcgu.o)
//! so the crate is what is between `lib` and the hash in the `.rlib` name.

use std::collections::BTreeMap;
use std::process::ExitCode;

/// The crate a map entry's `In` column belongs to.
fn crate_of(inp: &str) -> String {
    // Only the object name matters: drop any leading directory.
    let base = inp.rsplit('/').next().unwrap_or(inp);
    // "libNAME-hash.rlib(obj…)" or "NAME-hash.o" or "obj…"
    let stem = base.split(".rlib(").next().unwrap_or(base);
    let stem = stem.strip_suffix(".rlib").unwrap_or(stem);
    let stem = stem.strip_prefix("lib").unwrap_or(stem);
    // Cut the 16-hex-digit disambiguator Cargo appends.
    match stem.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            name.to_string()
        }
        _ => stem.to_string(),
    }
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: wasm_map <map file> [top]");
        return ExitCode::FAILURE;
    };
    let top: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // (crate, section) -> bytes; and section -> bytes as the linker reports it.
    let mut by_crate: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut sections: Vec<(String, u64)> = Vec::new();
    let mut section = String::from("?");
    let mut objects: BTreeMap<String, u64> = BTreeMap::new();
    let mut entries = 0u64;

    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() == 4 {
            // A section summary line: "- off size NAME".
            let name = f[3];
            if name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                if let Ok(size) = u64::from_str_radix(f[2], 16) {
                    sections.push((name.to_string(), size));
                    section = name.to_string();
                    continue;
                }
            }
        }
        // An entry line is the one that still carries the object path; the map
        // writes it as `/…/libfoo-hash.rlib(foo-hash.cgu.0.rcgu.o):(symbol)`.
        let Some(i) = f.iter().position(|w| w.contains(".o):(")) else {
            continue;
        };
        let Some(size) = i.checked_sub(1).and_then(|j| u64::from_str_radix(f[j], 16).ok()) else {
            continue;
        };
        entries += 1;
        let krate = crate_of(f[i]);
        *by_crate
            .entry(krate.clone())
            .or_default()
            .entry(section.clone())
            .or_default() += size;
        *objects.entry(krate).or_default() += size;
    }

    println!("map: {path}");
    println!("entries: {entries}\n");
    println!("sections (as the linker reports them):");
    let mut total = 0u64;
    for (name, size) in &sections {
        total += size;
        println!("  {size:>9}  {name}");
    }
    println!("  {total:>9}  total\n");

    let overall: u64 = objects.values().sum();
    let mut ranked: Vec<(String, u64)> = objects.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    println!("by crate (of {overall} bytes attributed):");
    for (krate, size) in ranked.iter().take(top) {
        let pct = 100.0 * *size as f64 / overall as f64;
        let parts = by_crate.get(krate).unwrap();
        let mut split: Vec<String> = parts
            .iter()
            .map(|(s, n)| format!("{s} {n}"))
            .collect();
        split.sort();
        println!(
            "  {size:>9}  {pct:>5.1} %  {:<24} {}",
            krate,
            split.join(", ")
        );
    }
    let rest: u64 = ranked.iter().skip(top).map(|(_, n)| *n).sum();
    if ranked.len() > top {
        println!("  {rest:>9}  {:>5.1} %  ({} smaller crates)", 100.0 * rest as f64 / overall as f64, ranked.len() - top);
    }
    ExitCode::SUCCESS
}
