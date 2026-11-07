//! Build the font bundle the browser fetches: every standard font the
//! renderer can fall back to, as one compressed resource.
//!
//! usage: cargo run --features bundle-write --bin fonts_blob -- <dir> [out-dir]
//!
//! `<dir>` is a `STANDARD_FONTS` directory: `fonts.json` plus the files it
//! names. That is all that goes in — not the licence texts, not the unused
//! `FoxitSerif*.pfb` / `FoxitFixed*.pfb`, which `fonts.json` does not name.
//!
//! The point of the blob is to be *one* fetch, so it is compressed as one
//! stream: fonts share an alphabet, a set of outline operators and, within a
//! family, many outlines. Every codec in `Codec` is written and measured, and
//! each one is read back and compared byte for byte against the files it came
//! from, because a font bundle that is subtly wrong is worse than a missing one.
//!
//! Brotli is the one to ship: smallest, and `brotli-decompressor` (pure Rust)
//! costs little more than the flate2 decoder the pdf crate already links. Gzip
//! is the fallback for a host with no brotli decoder at all — a browser can
//! inflate it with `DecompressionStream("gzip")` and hand the bytes to wasm.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdf_render::{Codec, DirRead, MemDir};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: fonts_blob <standard-fonts-dir> [out-dir]";
    let dir = match args.next() {
        Some(d) => PathBuf::from(d),
        None => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    let out_dir = args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = run(&dir, &out_dir) {
        eprintln!("fonts_blob: {e}");
        std::process::exit(1);
    }
}

fn run(dir: &Path, out_dir: &Path) -> Result<(), String> {
    let names = font_files(dir)?;

    let mut files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let mut total = 0;
    for name in &names {
        let data = std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
        if name != "fonts.json" {
            check_magic(name, &data)?;
        }
        total += data.len();
        files.insert(name.clone(), Arc::from(data));
    }

    println!("{dir:?}: {} file(s), {total} bytes", files.len());
    let mut by_size: Vec<(&String, &Arc<[u8]>)> = files.iter().collect();
    by_size.sort_by_key(|(_, d)| std::cmp::Reverse(d.len()));
    for (name, data) in by_size {
        println!("  {:>9}  {name}", data.len());
    }

    // One stream per file is what a font is normally compressed with. That sum
    // is the number a bundle has to beat — and the reason a bundle is worth
    // building at all, if it does.
    use std::io::Write;
    let mut per_file_gzip = 0;
    let mut per_file_brotli = 0;
    for data in files.values() {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(data).map_err(|e| e.to_string())?;
        per_file_gzip += e.finish().map_err(|e| e.to_string())?.len();

        let mut out = Vec::new();
        let mut src = &data[..];
        brotli::BrotliCompress(
            &mut src,
            &mut out,
            &brotli::enc::BrotliEncoderParams { quality: 11, ..Default::default() },
        )
        .map_err(|e| e.to_string())?;
        per_file_brotli += out.len();
    }

    let mem = MemDir::new(files.iter().map(|(k, v)| (k.clone(), v.clone())));

    // Brotli has a mode for font data (the one WOFF2 uses). Worth a line of its
    // own: if it beats generic, the shipped blob is built with it.
    let plain = mem.write_plain();
    for (label, mode) in [
        ("generic", brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_GENERIC),
        ("font", brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_FONT),
    ] {
        let mut out = Vec::new();
        let mut src = &plain[..];
        brotli::BrotliCompress(
            &mut src,
            &mut out,
            &brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 22, mode, ..Default::default() },
        )
        .map_err(|e| e.to_string())?;
        println!("brotli q11 lgwin22, {label} mode: {} bytes", out.len());
    }

    println!();
    println!("{:>9}  {:>7}  codec", "bytes", "of raw");
    let mut best = (usize::MAX, Codec::Store);
    for codec in [Codec::Brotli, Codec::Gzip, Codec::Deflate, Codec::Store] {
        let blob = mem.write_bundle(codec);
        let path = out_dir.join(format!("standard_fonts.{}.bundle", ext(codec)));
        std::fs::write(&path, &blob).map_err(|e| format!("{}: {e}", path.display()))?;
        verify(&blob, &files)?;
        println!(
            "{:>9}  {:>6.1}%  {codec:?}  ({})",
            blob.len(),
            100.0 * blob.len() as f64 / total as f64,
            path.display(),
        );
        if blob.len() < best.0 {
            best = (blob.len(), codec);
        }
    }
    println!("per-file gzip, for comparison: {per_file_gzip} bytes");
    println!("per-file brotli, for comparison: {per_file_brotli} bytes");
    println!();
    println!(
        "ship {:?}: {} bytes, {:.1}% of the files, {:.2}x the per-file gzip",
        best.1,
        best.0,
        100.0 * best.0 as f64 / total as f64,
        best.0 as f64 / per_file_gzip as f64,
    );
    Ok(())
}

/// The files a `STANDARD_FONTS` directory is: `fonts.json` and whatever it
/// names. Read as JSON rather than as a fixed list, so swapping the set (the
/// Foxit serif/mono designs, say) needs no change here.
fn font_files(dir: &Path) -> Result<Vec<String>, String> {
    let json = std::fs::read(dir.join("fonts.json")).map_err(|e| format!("fonts.json: {e}"))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&json).map_err(|e| format!("fonts.json: {e}"))?;
    let mut names: Vec<String> = vec!["fonts.json".into()];
    for value in map.values() {
        let name = value.as_str().ok_or("fonts.json: a value is not a file name")?;
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// A file whose first bytes are not a font would parse as garbage much later,
/// inside the renderer, on someone else's machine.
fn check_magic(name: &str, data: &[u8]) -> Result<(), String> {
    let head4 = data.get(..4).unwrap_or(data);
    let ok = match head4 {
        b"OTTO" | b"true" | b"ttcf" => true,
        [0, 1, 0, 0] => true,
        // Type 1: the PFB segment header, or a PFA, which is ASCII.
        _ => {
            data.get(..2) == Some(&[0x80, 0x01])
                || data.get(..2) == Some(&[0x80, 0x02])
                || data.starts_with(b"%!F")
                // A bare CFF table, no OpenType wrapper: the Foxit `*.pfb`
                // files are this, despite the name. CFF starts with
                // major 1, minor 0, header size 4, offset size 1..=4.
                || matches!(head4, [1, 0, 4, 1..=4])
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{name}: not an OpenType, TrueType, CFF or Type 1 font (starts with {head4:02x?})"
        ))
    }
}

/// Read the blob back and compare it with what went in.
fn verify(blob: &[u8], files: &BTreeMap<String, Arc<[u8]>>) -> Result<(), String> {
    let back = MemDir::parse_bundle(blob).map_err(|e| format!("reading it back: {e}"))?;
    if back.len() != files.len() {
        return Err(format!("{} files back, {} went in", back.len(), files.len()));
    }
    for (name, want) in files {
        let got = back.read_file(name).map_err(|e| format!("{name}: {e}"))?;
        if &*got != &**want {
            return Err(format!("{name}: {} bytes back, {} went in", got.len(), want.len()));
        }
    }
    Ok(())
}

fn ext(codec: Codec) -> &'static str {
    match codec {
        Codec::Store => "plain",
        Codec::Brotli => "brotli",
        Codec::Deflate => "deflate",
        Codec::Gzip => "gzip",
    }
}
