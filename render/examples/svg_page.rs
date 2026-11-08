//! Render a PDF page to SVG with the text-preserving SVG backend.
//!
//! usage: cargo run --features svg --example svg_page -- <file.pdf> <page> <out.svg>
//!
//! `page` is 1-based. A page becomes a stack of layers, bottom to top: the
//! vector layers are SVG documents, images are raw RGBA the host draws itself.
//! Here they are written next to each other as `out.svg`, `out.1.svg`, ... plus
//! a `out.images` manifest line per image, which is what the browser side gets
//! instead of pixels.

use pdf::file::FileOptions;
use pdf_render::svg_backend::{Layer, SvgBackend};
use pdf_render::{render_page, Cache};
use pathfinder_geometry::transform2d::Transform2F;

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: svg_page <file.pdf> <page> <out.svg>");
        return;
    }
    let path = &args[1];
    let page_nr: u32 = args[2].parse().expect("page must be a number");
    let out = &args[3];

    // Same convention as the other examples: run from the crate directory and
    // the fonts are here, otherwise $STANDARD_FONTS has to say where they are.
    if std::env::var_os("STANDARD_FONTS").is_none() {
        let local = if std::path::Path::new("fonts").is_dir() { "fonts" } else { "standard_fonts" };
        if std::path::Path::new(local).is_dir() {
            std::env::set_var("STANDARD_FONTS", local);
        }
    }

    let file = FileOptions::cached().open(path).expect("cannot open the file");
    let resolver = file.resolver();
    let mut cache = Cache::new(pdf_render::svg_backend::OutlineBuilder::default());
    let page = file.get_page(page_nr - 1).expect("page index out of range");

    let mut backend = SvgBackend::new(&mut cache);
    render_page(&mut backend, &resolver, &page, Transform2F::default())
        .unwrap_or_else(|e| panic!("page {page_nr} failed: {e:?}"));
    let spans = backend.text_span_count();
    let skipped = backend.inline_images_skipped();
    let (glyphs, paths) = backend.counts();
    let page = backend.finish();
    println!("  glyphs {glyphs}, paths {paths}");

    println!(
        "page {}: {}x{} pt, {} layer(s), {} text spans, {} inline image(s) skipped",
        page_nr,
        page.width(),
        page.height(),
        page.layers.len(),
        spans,
        skipped
    );

    let stem = out.strip_suffix(".svg").unwrap_or(out);
    let mut images = String::new();
    for (i, layer) in page.layers.iter().enumerate() {
        match layer {
            Layer::Vector(svg) => {
                let name = if i == 0 { format!("{stem}.svg") } else { format!("{stem}.{i}.svg") };
                std::fs::write(&name, svg).expect("cannot write svg");
                println!("  layer {i}: vector, {} bytes -> {name}", svg.len());
            }
            Layer::Image(im) => {
                let name = format!("{stem}.{i}.rgba");
                std::fs::write(&name, &im.rgba[..]).expect("cannot write rgba");
                images.push_str(&format!(
                    "layer {i}: {}x{} rgba -> {name} matrix {:?} clip {}\n",
                    im.width,
                    im.height,
                    im.matrix,
                    im.clip.is_some(),
                ));
                println!("  layer {i}: image {}x{} -> {name}", im.width, im.height);
            }
        }
    }
    if !images.is_empty() {
        let name = format!("{stem}.images");
        std::fs::write(&name, images).expect("cannot write image manifest");
        println!("  image manifest -> {name}");
    }
}
