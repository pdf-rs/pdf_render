//! Walk a file page by page the way the browser does — ask, fetch, supply,
//! ask again — and say what happened on each page. The point is the failures:
//! the viewer only shows a stack, so this prints the reason.
//!
//!     cargo run -p pdf_web --example rangecheck -- FILE.pdf [MAX_PAGES]

use std::sync::Arc;

use pdf_render::MemDir;
use pdf_web::{Doc, Ready};

fn fonts() -> Option<Vec<u8>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../standard_fonts");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "README.md" {
            continue;
        }
        let data: Arc<[u8]> = std::fs::read(entry.path()).ok()?.into();
        files.push((name, data));
    }
    (!files.is_empty()).then(|| MemDir::new(files).write_plain())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let limit: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let bytes = std::fs::read(path).expect("read");
    let mut doc = Doc::new(bytes.len() as u32);
    if let Some(bundle) = fonts() {
        doc.set_fonts(&bundle).expect("fonts");
    }

    let mut tripped = 0;
    let mut pages = 0;
    // Open: page count first, as the page does.
    let mut rounds = 0;
    loop {
        rounds += 1;
        match doc.prepare(u32::MAX) {
            Ready::Need(ranges) => {
                for r in ranges {
                    doc.supply(r.start as u32, &bytes[r]);
                }
            }
            Ready::Opened(n) => {
                pages = n as usize;
                break;
            }
            Ready::Failed => {
                println!("open: FAILED: {}", doc.error().unwrap_or("no reason"));
                std::process::exit(1);
            }
            _ => {
                println!("open: unexpected ready");
                std::process::exit(1);
            }
        }
        if rounds > 32 {
            println!("open: did not settle");
            std::process::exit(1);
        }
    }

    let mut worst = 0usize;
    for page in 0..pages.min(limit) {
        let mut round = 0;
        let outcome = loop {
            round += 1;
            if round > 32 {
                break Err("did not settle in 32 rounds".to_string());
            }
            match doc.prepare(page as u32) {
                Ready::Need(ranges) => {
                    for r in ranges {
                        if r.end > bytes.len() {
                            break;
                        }
                        doc.supply(r.start as u32, &bytes[r]);
                    }
                }
                Ready::Page => break Ok(round),
                Ready::Failed => {
                    break Err(doc.error().unwrap_or("failed without a reason").to_string())
                }
                _ => break Err("unexpected ready".to_string()),
            }
        };
        worst = worst.max(round);
        match outcome {
            Ok(n) => {
                let svg = doc.take_svg(page as u32).unwrap_or_default();
                if std::env::var_os("VERBOSE").is_some() {
                    println!("page {:3}: ok in {n} round(s), {} bytes svg", page + 1, svg.len());
                }
            }
            Err(e) => {
                tripped += 1;
                let svg = doc.take_svg(page as u32);
                println!(
                    "page {:3}: FAILED after {round} round(s): {e} (svg {})",
                    page + 1,
                    svg.map(|s| s.len().to_string()).unwrap_or_else(|| "-".into())
                );
            }
        }
    }
    println!(
        "{path}: {pages} page(s), {tripped} failed, worst {worst} round(s)"
    );
}
