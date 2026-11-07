//! Where the bytes are: per-table sizes of the standard fonts, and what a
//! stripped variant compresses to.
//!
//! usage: cargo run --features bundle-write --bin fonts_probe -- <dir>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: fonts_probe <dir>");
        std::process::exit(2)
    });
    let r = run(&dir);
    if let Err(e) = r {
        eprintln!("fonts_probe: {e}");
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
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            matches!(ext, "otf" | "ttf" | "pfb")
        })
        .collect();
    entries.sort();

    let mut per_table: BTreeMap<String, usize> = BTreeMap::new();
    let mut raw_total = 0;
    let mut files = Vec::new();
    for path in &entries {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        raw_total += data.len();
        // The Foxit `*.pfb` are bare CFF, no sfnt wrapper: nothing to strip.
        let tables = if path.extension().and_then(|s| s.to_str()) == Some("pfb") {
            vec![("CFF ".to_string(), 0usize, data.len())]
        } else {
            sfnt_tables(&data)?
        };
        for (tag, off, len) in &tables {
            *per_table.entry(tag.clone()).or_default() += len;
            let _ = off;
        }
        println!(
            "{:>9}  {}  ({} tables)",
            data.len(),
            path.file_name().unwrap().to_string_lossy(),
            tables.len()
        );
        files.push((path.file_name().unwrap().to_string_lossy().to_string(), data, tables));
    }
    println!("\nraw total {raw_total}");

    println!("\n{:>10}  table", "bytes");
    let mut t: Vec<(&String, &usize)> = per_table.iter().collect();
    t.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (tag, n) in t {
        println!("{n:>10}  {tag}");
    }

    // Drop what a PDF renderer never reads: hinting, shaping, metadata.
    let drop_tags = [
        "DSIG", "gasp", "fpgm", "prep", "cvt ", "hdmx", "LTSH", "VDMX", "GSUB", "GPOS", "GDEF",
        "kern", "name", "OS/2", "post", "MATH", "BASE", "JSTF", "VDMX", "FFTM", "BDF ", "meta",
    ];
    let mut kept: Vec<(String, Vec<u8>)> = Vec::new();
    let mut kept_total = 0;
    for (name, _data, tables) in &files {
        let stripped = rebuild(_data, tables, &drop_tags)?;
        kept_total += stripped.len();
        kept.push((name.clone(), stripped));
    }
    println!("\nstripped total {kept_total} (was {raw_total})");

    // brotli over the stripped set, one stream, same order as the bundle.
    let mut concat = Vec::new();
    for (_, d) in &kept {
        concat.extend_from_slice(d);
    }
    println!("brotli(stripped set, one stream) {}", brotli_len(&concat));

    let mut concat_raw = Vec::new();
    for (_, d, _) in &files {
        concat_raw.extend_from_slice(d);
    }
    println!("brotli(raw set, one stream)      {}", brotli_len(&concat_raw));
    Ok(())
}

/// (tag, offset, length) of every sfnt table, in file order.
fn sfnt_tables(data: &[u8]) -> Result<Vec<(String, usize, usize)>, String> {
    if data.len() < 12 {
        return Err("too short".into());
    }
    let n = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut out = Vec::new();
    for i in 0..n {
        let p = 12 + i * 16;
        let rec = data.get(p..p + 16).ok_or("truncated table directory")?;
        let tag = String::from_utf8_lossy(&rec[0..4]).to_string();
        let off = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
        let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
        out.push((tag, off, len));
    }
    Ok(out)
}

fn rebuild(
    data: &[u8],
    tables: &[(String, usize, usize)],
    drop_tags: &[&str],
) -> Result<Vec<u8>, String> {
    let keep: Vec<&(String, usize, usize)> =
        tables.iter().filter(|(t, _, _)| !drop_tags.contains(&t.as_str())).collect();
    let n = keep.len();
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[0..4]);
    out.extend_from_slice(&(n as u16).to_be_bytes());
    // searchRange etc. are advisory; keep the arithmetic honest anyway.
    let mut sr = 1usize;
    let mut es = 0u16;
    while sr * 2 <= n {
        sr *= 2;
        es += 1;
    }
    out.extend_from_slice(&((sr * 16) as u16).to_be_bytes());
    out.extend_from_slice(&es.to_be_bytes());
    out.extend_from_slice(&((n * 16 - sr * 16) as u16).to_be_bytes());
    let dir_start = out.len();
    out.resize(dir_start + n * 16, 0);
    let mut body = Vec::new();
    for (i, (tag, off, len)) in keep.iter().enumerate() {
        let rec = dir_start + i * 16;
        out[rec..rec + 4].copy_from_slice(tag.as_bytes());
        out[rec + 4..rec + 8].copy_from_slice(&[0, 0, 0, 0]);
        let new_off = dir_start + n * 16 + body.len();
        out[rec + 8..rec + 12].copy_from_slice(&(new_off as u32).to_be_bytes());
        out[rec + 12..rec + 16].copy_from_slice(&(*len as u32).to_be_bytes());
        let chunk = data.get(*off..off + len).ok_or("table past end")?;
        body.extend_from_slice(chunk);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    out.extend_from_slice(&body);
    Ok(out)
}
