//! Build a vello scene for the pages of a PDF and print what went into it.
//!
//! This needs no GPU: it stops before rasterisation, so it verifies that the
//! backend's drawing calls (paths, clips, images, glyphs) survive a vello
//! upgrade at runtime, not just at compile time.
//!
//! usage: cargo run --example vello_scene -- <file.pdf> [max-pages]

use pdf::file::FileOptions;
use pdf_render::render_page;
use pdf_render::vello_backend::{OutlineBuilder, VelloBackend};
use pdf_render::Cache;

fn main() {
    env_logger::init();
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: vello_scene <file.pdf> [max-pages]");
            return;
        }
    };
    let max_pages: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let file = FileOptions::cached().open(&path).expect("cannot open the file");
    let resolver = file.resolver();
    // Cache::new insists on a directory of standard fonts; an empty one is
    // enough as long as the document embeds its fonts.
    if std::env::var_os("STANDARD_FONTS").is_none() && !std::path::Path::new("fonts").is_dir() {
        std::env::set_var("STANDARD_FONTS", std::env::temp_dir());
    }
    let mut cache = Cache::<OutlineBuilder>::new(OutlineBuilder::default());

    let mut pages = 0u32;
    for (i, page) in file.pages().enumerate() {
        if i >= max_pages {
            break;
        }
        let page = page.expect("cannot read page");
        let mut backend = VelloBackend::new(&mut cache);
        render_page(&mut backend, &resolver, &page, Default::default())
            .unwrap_or_else(|e| panic!("page {} failed: {:?}", i + 1, e));
        let scene = backend.finish();
        let encoding = scene.encoding();
        println!(
            "page {}: {} path tags, {} draw tags, {} transforms",
            i + 1,
            encoding.path_tags.len(),
            encoding.draw_tags.len(),
            encoding.transforms.len(),
        );
        pages += 1;
    }
    println!("{} page(s) built", pages);
}
