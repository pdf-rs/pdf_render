//! Where the CFF bytes are: charstrings vs. subroutines vs. the rest. A subset
//! only removes charstrings until the subroutines it no longer calls are pruned
//! too, so this is the number that decides what a subset can save.
//!
//! usage: cargo run --features bundle-write --bin fonts_parts -- <dir>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: fonts_parts <dir>");
        std::process::exit(2)
    });
    if let Err(e) = run(&dir) {
        eprintln!("fonts_parts: {e}");
        std::process::exit(1);
    }
}

fn brotli_len(data: &[u8]) -> usize {
    let mut out = Vec::new();
    let mut src = data;
    brotli::BrotliCompress(
        &mut src,
        &mut out,
        &brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 22, ..Default::default() },
    )
    .unwrap();
    out.len()
}

fn run(dir: &Path) -> Result<(), String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("otf") | Some("ttf")))
        .collect();
    paths.sort();

    let mut by_cat: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for p in &paths {
        let data = std::fs::read(p).map_err(|e| e.to_string())?;
        let tables = sfnt_tables(&data)?;
        let (cff_off, cff_len) = tables
            .iter()
            .find(|(t, _, _)| t == "CFF ")
            .map(|(_, o, l)| (*o, *l))
            .ok_or("no CFF")?;
        let cff = &data[cff_off..cff_off + cff_len];
        let parts = breakdown(cff)?;
        let total: usize = parts.iter().map(|(_, b)| b.len()).sum();
        if total != cff.len() {
            println!(
                "{}: parts sum {total}, CFF is {} (tail unaccounted)",
                p.file_name().unwrap().to_string_lossy(),
                cff.len()
            );
        }
        for (name, bytes) in parts {
            by_cat.entry(name).or_default().extend_from_slice(&bytes);
        }
    }

    let mut rows: Vec<(&String, usize, usize)> =
        by_cat.iter().map(|(k, v)| (k, v.len(), brotli_len(v))).collect();
    rows.sort_by_key(|(_, raw, _)| std::cmp::Reverse(*raw));
    let raw_all: usize = rows.iter().map(|r| r.1).sum();
    let comp_all: usize = rows.iter().map(|r| r.2).sum();
    println!("\n{:<16} {:>10} {:>10}", "part", "raw", "brotli");
    for (name, raw, comp) in &rows {
        println!("{name:<16} {raw:>10} {comp:>10}");
    }
    println!("{:<16} {raw_all:>10} {comp_all:>10}  (per part, separate streams)", "sum");
    let mut joined = Vec::new();
    for name in by_cat.keys() {
        joined.extend_from_slice(&by_cat[name]);
    }
    println!("{:<16} {raw_all:>10} {:>10}  (one stream)", "sum", brotli_len(&joined));
    Ok(())
}

fn sfnt_tables(data: &[u8]) -> Result<Vec<(String, usize, usize)>, String> {
    let n = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut out = Vec::new();
    for i in 0..n {
        let p = 12 + i * 16;
        let rec = data.get(p..p + 16).ok_or("truncated table directory")?;
        out.push((
            String::from_utf8_lossy(&rec[0..4]).to_string(),
            u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize,
            u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize,
        ));
    }
    Ok(out)
}

fn index_count(data: &[u8], at: usize) -> usize {
    u16::from_be_bytes([data[at], data[at + 1]]) as usize
}

fn read_off(data: &[u8], at: usize, size: usize) -> usize {
    let mut v = 0usize;
    for i in 0..size {
        v = (v << 8) | data[at + i] as usize;
    }
    v
}

/// Whole size of the INDEX at `at`, and the data slices it holds.
fn index_span(data: &[u8], at: usize) -> (usize, usize) {
    let count = index_count(data, at);
    if count == 0 {
        return (2, at + 2);
    }
    let off_size = data[at + 2] as usize;
    let offs = at + 3;
    let base = offs + (count + 1) * off_size - 1;
    let end = base + read_off(data, offs + count * off_size, off_size);
    (end - at, end)
}

fn dict_ops(dict: &[u8]) -> Vec<(u16, Vec<i32>)> {
    let mut out = Vec::new();
    let mut stack: Vec<i32> = Vec::new();
    let mut i = 0;
    while i < dict.len() {
        let b = dict[i];
        i += 1;
        match b {
            32..=246 => stack.push(b as i32 - 139),
            247..=250 => {
                stack.push((b as i32 - 247) * 256 + dict[i] as i32 + 108);
                i += 1;
            }
            251..=254 => {
                stack.push(-(b as i32 - 251) * 256 - dict[i] as i32 - 108);
                i += 1;
            }
            28 => {
                stack.push(i16::from_be_bytes([dict[i], dict[i + 1]]) as i32);
                i += 2;
            }
            29 => {
                stack.push(i32::from_be_bytes([dict[i], dict[i + 1], dict[i + 2], dict[i + 3]]));
                i += 4;
            }
            30 => {
                while i < dict.len() {
                    let b = dict[i];
                    i += 1;
                    if b & 0x0f == 0x0f || b >> 4 == 0x0f {
                        break;
                    }
                }
                stack.push(0);
            }
            op => {
                let op = if op == 12 {
                    i += 1;
                    0x0c00 | dict[i - 1] as u16
                } else {
                    op as u16
                };
                out.push((op, std::mem::take(&mut stack)));
            }
        }
    }
    out
}

fn breakdown(cff: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let hdr = cff[2] as usize;
    let mut at = hdr;
    let mut out = vec![("header".to_string(), cff[..hdr].to_vec())];

    let (span, next) = index_span(cff, at);
    out.push(("name".into(), cff[at..at + span].to_vec()));
    at = next;
    let (span, next) = index_span(cff, at);
    let top = &cff[at..at + span];
    out.push(("topdict".into(), top.to_vec()));
    at = next;
    let (span, next) = index_span(cff, at);
    out.push(("strings".into(), cff[at..at + span].to_vec()));
    at = next;
    let (span, next) = index_span(cff, at);
    out.push(("gsubrs".into(), cff[at..at + span].to_vec()));
    let _ = next;

    // The single font's Top DICT, re-parsed from its INDEX.
    let td_off = hdr + index_span(cff, hdr).0;
    let count = index_count(cff, td_off);
    if count != 1 {
        return Err(format!("{count} top dicts"));
    }
    let top = dict_ops(&top[3 + 2 * 1..]); // past count + offSize + offsets
    let mut charstrings = 0usize;
    let mut charset = 0usize;
    let mut privates: Vec<(usize, usize)> = Vec::new();
    for (op, args) in &top {
        match (op, args.as_slice()) {
            (17, [off]) => charstrings = *off as usize,
            (15, [off]) => charset = *off as usize,
            (18, [size, off]) => privates.push((*off as usize, *size as usize)),
            _ => {}
        }
    }

    let (span, _) = index_span(cff, charstrings);
    out.push(("charstrings".into(), cff[charstrings..charstrings + span].to_vec()));

    // Charset: format byte, then ranges; its length follows from the glyph
    // count, which is the CharStrings count.
    let n_glyphs = index_count(cff, charstrings);
    let cs_len = match cff[charset] {
        0 => 1 + (n_glyphs - 1) * 2,
        f @ (1 | 2) => {
            let mut i = charset + 1;
            let mut seen = 1;
            while seen < n_glyphs {
                seen += if f == 1 {
                    let n = cff[i + 2] as usize;
                    i += 3;
                    n + 1
                } else {
                    let n = u16::from_be_bytes([cff[i + 2], cff[i + 3]]) as usize;
                    i += 4;
                    n + 1
                };
            }
            i - charset
        }
        f => return Err(format!("charset format {f}")),
    };
    out.push(("charset".into(), cff[charset..charset + cs_len].to_vec()));

    for (off, size) in privates {
        let dict = &cff[off..off + size];
        let mut spans = vec![("private".to_string(), dict.to_vec())];
        for (op, args) in dict_ops(dict) {
            if op == 19 {
                if let [rel] = args.as_slice() {
                    let at = off + *rel as usize;
                    let (span, _) = index_span(cff, at);
                    spans.push(("lsubrs".into(), cff[at..at + span].to_vec()));
                }
            }
        }
        out.extend(spans);
    }
    Ok(out)
}
