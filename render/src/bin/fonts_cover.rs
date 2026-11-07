//! Where the outline bytes sit: per-glyph CFF charstring sizes, bucketed by
//! Unicode coverage. Answers "how big is the part a PDF encoding can reach".
//!
//! usage: cargo run --features bundle-write --bin fonts_cover -- <dir>

use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: fonts_cover <dir>");
        std::process::exit(2)
    });
    if let Err(e) = run(&dir) {
        eprintln!("fonts_cover: {e}");
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

/// Cumulative repertoires, narrowest first. Each is a predicate on a scalar
/// value: is this codepoint in the set.
fn repertoires() -> Vec<(&'static str, fn(u32) -> bool)> {
    vec![
        ("ASCII 20-7E", |c| (0x20..=0x7e).contains(&c)),
        ("+Latin-1 A0-FF", |c| (0x20..=0x7e).contains(&c) || (0xa0..=0xff).contains(&c)),
        (
            "+Latin Ext-A 100-17F",
            |c| {
                (0x20..=0x7e).contains(&c)
                    || (0xa0..=0x17f).contains(&c)
                    || matches!(c, 0x2010..=0x203a | 0x20ac | 0x2122)
            },
        ),
        (
            "+Greek 370-3FF",
            |c| {
                (0x20..=0x7e).contains(&c)
                    || (0xa0..=0x17f).contains(&c)
                    || matches!(c, 0x2010..=0x203a | 0x20ac | 0x2122)
                    || (0x370..=0x3ff).contains(&c)
            },
        ),
        (
            "+Cyrillic 400-4FF",
            |c| {
                (0x20..=0x7e).contains(&c)
                    || (0xa0..=0x17f).contains(&c)
                    || matches!(c, 0x2010..=0x203a | 0x20ac | 0x2122)
                    || (0x370..=0x3ff).contains(&c)
                    || (0x400..=0x4ff).contains(&c)
            },
        ),
    ]
}

struct Font {
    name: String,
    /// Charstring bytes per glyph id.
    glyph: Vec<Vec<u8>>,
    /// Codepoints mapped to each glyph id.
    cps: Vec<Vec<u32>>,
}

impl Font {
    fn cs_len(&self) -> usize {
        self.glyph.iter().map(|g| g.len()).sum()
    }
}

fn run(dir: &Path) -> Result<(), String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("otf") | Some("ttf")))
        .collect();
    paths.sort();

    let mut fonts = Vec::new();
    for p in &paths {
        let data = std::fs::read(p).map_err(|e| e.to_string())?;
        fonts.push(read_font(&data, p.file_name().unwrap().to_string_lossy().to_string())?);
    }

    let reps = repertoires();
    println!(
        "{:<26} {:>6} {:>7} {:>9}  cumulative charstring bytes kept (%)",
        "font", "glyphs", "unmapped", "cs bytes"
    );
    let mut totals = vec![0usize; reps.len()];
    let mut total_cs = 0;
    let mut total_glyphs = 0;
    let mut total_mapped = 0;
    for f in &fonts {
        let total = f.cs_len();
        total_cs += total;
        total_glyphs += f.glyph.len();
        let unmapped = f.cps.iter().filter(|v| v.is_empty()).count();
        total_mapped += f.glyph.len() - unmapped;
        let mut line = format!(
            "{:<26} {:>6} {:>7} {:>9}",
            f.name.trim_end_matches(".otf"),
            f.glyph.len(),
            unmapped,
            total
        );
        for (i, (_, pred)) in reps.iter().enumerate() {
            let mut kept = 0;
            for (gid, cps) in f.cps.iter().enumerate() {
                if cps.iter().any(|&c| pred(c)) {
                    kept += f.glyph[gid].len();
                }
            }
            totals[i] += kept;
            line += &format!("  {:>6.1}%", 100.0 * kept as f64 / total as f64);
        }
        println!("{line}");
    }
    println!(
        "\ntotal: {} glyphs, {} mapped, {} charstring bytes",
        total_glyphs, total_mapped, total_cs
    );

    // What the same selection costs compressed. The bundle is one brotli
    // stream, so the outlines' share of it is roughly their own stream's size:
    // estimate the subset bundle as the whole one minus the outlines removed.
    let all_cs = {
        let mut v = Vec::new();
        for f in &fonts {
            for g in &f.glyph {
                v.extend_from_slice(g);
            }
        }
        brotli_len(&v)
    };
    println!("outlines of all fonts, brotli: {all_cs} bytes\n");
    println!("{:<22} {:>10} {:>9} {:>12} {:>10}", "repertoire", "outline B", "brotli", "est. bundle", "vs 547313");
    for (i, (name, pred)) in reps.iter().enumerate() {
        let mut raw = Vec::new();
        for f in &fonts {
            for (gid, cps) in f.cps.iter().enumerate() {
                if cps.iter().any(|&c| pred(c)) {
                    raw.extend_from_slice(&f.glyph[gid]);
                }
            }
        }
        let comp = brotli_len(&raw);
        let est = 547_313 - all_cs + comp;
        println!(
            "{:<22} {:>10} {:>9} {:>12} {:>9.0}%",
            name,
            raw.len(),
            comp,
            est,
            100.0 * est as f64 / 547_313.0
        );
    }
    Ok(())
}

fn read_font(data: &[u8], name: String) -> Result<Font, String> {
    let tables = sfnt_tables(data)?;
    let table = |tag: &str| -> Option<&[u8]> {
        tables
            .iter()
            .find(|(t, _, _)| t == tag)
            .and_then(|(_, o, l)| data.get(*o..o + l))
    };
    let cmap = table("cmap").ok_or("no cmap")?;
    let cff = table("CFF ").ok_or("no CFF")?;
    let maxp = table("maxp").ok_or("no maxp")?;
    let n_glyphs = u16::from_be_bytes([maxp[4], maxp[5]]) as usize;

    let cs = cff_charstrings(cff)?;
    if cs.len() != n_glyphs {
        return Err(format!("{name}: {} charstrings, maxp says {n_glyphs}", cs.len()));
    }
    let mut cps = vec![Vec::new(); n_glyphs];
    for (cp, gid) in cmap_entries(cmap)? {
        if (gid as usize) < n_glyphs {
            cps[gid as usize].push(cp);
        }
    }
    Ok(Font { name, glyph: cs, cps })
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

/// The CharStrings INDEX at the Top DICT's CharStrings offset, whole items:
/// a subset keeps each glyph byte for byte, so its size is its whole cost.
fn cff_charstrings(cff: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let hdr_size = cff[2] as usize;
    let (name_idx_end, _) = index_end(cff, hdr_size)?;
    let (top_dict, top_end) = index_items(cff, name_idx_end)?;
    let (_, string_end) = index_end(cff, top_end)?;
    let (_, subr_end) = index_end(cff, string_end)?;
    let _ = subr_end;

    let mut charstrings_off = None;
    for d in &top_dict {
        let mut stack: Vec<i32> = Vec::new();
        let mut i = 0;
        while i < d.len() {
            let b = d[i];
            i += 1;
            match b {
                32..=246 => stack.push(b as i32 - 139),
                247..=250 => {
                    stack.push((b as i32 - 247) * 256 + *d.get(i).ok_or("dict")? as i32 + 108);
                    i += 1;
                }
                251..=254 => {
                    stack.push(-(b as i32 - 251) * 256 - *d.get(i).ok_or("dict")? as i32 - 108);
                    i += 1;
                }
                28 => {
                    let v = i16::from_be_bytes([d[i], d[i + 1]]) as i32;
                    stack.push(v);
                    i += 2;
                }
                29 => {
                    let v = i32::from_be_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
                    stack.push(v);
                    i += 4;
                }
                30 => {
                    // real: skip until 0xf terminator nibble
                    while i < d.len() {
                        let b = d[i];
                        i += 1;
                        if b & 0x0f == 0x0f || b >> 4 == 0x0f {
                            break;
                        }
                    }
                    stack.push(0);
                }
                op => {
                    if op == 17 {
                        charstrings_off = stack.last().copied();
                    }
                    if op == 12 {
                        i += 1;
                    }
                    stack.clear();
                }
            }
        }
    }
    let off = charstrings_off.ok_or("no CharStrings")? as usize;
    let items = index_items(cff, off)?.0;
    Ok(items.into_iter().map(|d| d.to_vec()).collect())
}

/// Offset just past the INDEX starting at `at`.
fn index_end(data: &[u8], at: usize) -> Result<(usize, usize), String> {
    let count = u16::from_be_bytes([data[at], data[at + 1]]) as usize;
    if count == 0 {
        return Ok((at + 2, at + 2));
    }
    let off_size = data[at + 2] as usize;
    let offs = at + 3;
    let last = read_off(data, offs + count * off_size, off_size)?;
    let base = offs + (count + 1) * off_size - 1;
    Ok((base + last, base + last))
}

fn read_off(data: &[u8], at: usize, size: usize) -> Result<usize, String> {
    let mut v = 0usize;
    for i in 0..size {
        v = (v << 8) | *data.get(at + i).ok_or("offset past end")? as usize;
    }
    Ok(v)
}

fn index_items(data: &[u8], at: usize) -> Result<(Vec<&[u8]>, usize), String> {
    let count = u16::from_be_bytes([data[at], data[at + 1]]) as usize;
    if count == 0 {
        return Ok((Vec::new(), at + 2));
    }
    let off_size = data[at + 2] as usize;
    let offs = at + 3;
    let base = offs + (count + 1) * off_size - 1;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let a = read_off(data, offs + i * off_size, off_size)?;
        let b = read_off(data, offs + (i + 1) * off_size, off_size)?;
        out.push(data.get(base + a..base + b).ok_or("index item past end")?);
    }
    Ok((out, base + read_off(data, offs + count * off_size, off_size)?))
}

fn cmap_entries(cmap: &[u8]) -> Result<Vec<(u32, u16)>, String> {
    let n = u16::from_be_bytes([cmap[2], cmap[3]]) as usize;
    let mut out = Vec::new();
    // Prefer the Unicode subtables; format 12 before 4 so the fuller one wins.
    let mut subs: Vec<(u32, usize)> = Vec::new();
    for i in 0..n {
        let p = 4 + i * 8;
        let plat = u16::from_be_bytes([cmap[p], cmap[p + 1]]);
        let enc = u16::from_be_bytes([cmap[p + 2], cmap[p + 3]]);
        let off = u32::from_be_bytes([cmap[p + 4], cmap[p + 5], cmap[p + 6], cmap[p + 7]]) as usize;
        let unicode = (plat == 0) || (plat == 3 && (enc == 1 || enc == 10));
        if unicode {
            let fmt = u16::from_be_bytes([cmap[off], cmap[off + 1]]);
            subs.push((fmt as u32, off));
        }
    }
    subs.sort();
    for (fmt, off) in subs {
        match fmt {
            4 => format4(cmap, off, &mut out)?,
            12 => format12(cmap, off, &mut out)?,
            _ => {}
        }
    }
    Ok(out)
}

fn format4(cmap: &[u8], at: usize, out: &mut Vec<(u32, u16)>) -> Result<(), String> {
    let segx2 = u16::from_be_bytes([cmap[at + 6], cmap[at + 7]]) as usize;
    let segs = segx2 / 2;
    let ends = at + 14;
    let starts = ends + segx2 + 2;
    let deltas = starts + segx2;
    let ranges = deltas + segx2;
    for s in 0..segs {
        let end = u16::from_be_bytes([cmap[ends + s * 2], cmap[ends + s * 2 + 1]]) as u32;
        let start = u16::from_be_bytes([cmap[starts + s * 2], cmap[starts + s * 2 + 1]]) as u32;
        let delta = u16::from_be_bytes([cmap[deltas + s * 2], cmap[deltas + s * 2 + 1]]);
        let ro = u16::from_be_bytes([cmap[ranges + s * 2], cmap[ranges + s * 2 + 1]]) as usize;
        if start > end {
            continue;
        }
        for c in start..=end {
            if c == 0xffff {
                continue;
            }
            let gid = if ro == 0 {
                (c as u16).wrapping_add(delta)
            } else {
                let idx = ranges + s * 2 + ro + 2 * (c - start) as usize;
                let g = u16::from_be_bytes([cmap[idx], cmap[idx + 1]]);
                if g == 0 {
                    0
                } else {
                    g.wrapping_add(delta)
                }
            };
            if gid != 0 {
                out.push((c, gid));
            }
        }
    }
    Ok(())
}

fn format12(cmap: &[u8], at: usize, out: &mut Vec<(u32, u16)>) -> Result<(), String> {
    let groups = u32::from_be_bytes([cmap[at + 12], cmap[at + 13], cmap[at + 14], cmap[at + 15]])
        as usize;
    for g in 0..groups {
        let p = at + 16 + g * 12;
        let start =
            u32::from_be_bytes([cmap[p], cmap[p + 1], cmap[p + 2], cmap[p + 3]]);
        let end = u32::from_be_bytes([cmap[p + 4], cmap[p + 5], cmap[p + 6], cmap[p + 7]]);
        let gid = u32::from_be_bytes([cmap[p + 8], cmap[p + 9], cmap[p + 10], cmap[p + 11]]);
        for c in start..=end.min(start + 4096) {
            out.push((c, (gid + (c - start)) as u16));
        }
    }
    Ok(())
}
