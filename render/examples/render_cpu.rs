//! Render PDF pages to PNG with the CPU backend — no GPU needed.
//!
//! usage: cargo run --features vello_cpu --example render_cpu -- <file.pdf> <page> [scale] [out.png]
//!
//! `page` is 1-based; `scale` is pixels per PDF unit (1.0 renders at the
//! document's natural size in millimetres at 72 dpi... see `page_bounds`).

use pdf::file::FileOptions;
use pdf_render::render_page;
use pdf_render::vello_cpu_backend::{OutlineBuilder, VelloCpuBackend};
use pdf_render::Cache;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::vec2f;

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: render_cpu <file.pdf> <page> [scale] [out.png]");
        return;
    }
    let path = &args[1];
    let page_nr: usize = args[2].parse().expect("page must be a number");
    let scale: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let out = args
        .get(4)
        .cloned()
        .unwrap_or_else(|| format!("page{}.png", page_nr));

    let file = FileOptions::cached().open(path).expect("cannot open the file");
    let resolver = file.resolver();
    if std::env::var_os("STANDARD_FONTS").is_none() && !std::path::Path::new("fonts").is_dir() {
        std::env::set_var("STANDARD_FONTS", std::env::temp_dir());
    }
    let mut cache = Cache::<OutlineBuilder>::new(OutlineBuilder::default());

    let page = file
        .get_page(page_nr as u32 - 1)
        .expect("page index out of range");
    let bounds = pdf_render::page_bounds(&page);
    let width = (bounds.width() * scale).ceil() as u16;
    let height = (bounds.height() * scale).ceil() as u16;
    println!("page {}: {}x{} px", page_nr, width, height);

    let mut backend = VelloCpuBackend::new(&mut cache, width, height);
    backend.fill_background(1.0, 1.0, 1.0);
    render_page(
        &mut backend,
        &resolver,
        &page,
        Transform2F::from_scale(vec2f(scale, scale)),
    )
    .unwrap_or_else(|e| panic!("page {} failed: {:?}", page_nr, e));

    let pixmap = backend.finish();
    let png = pixmap.into_png().expect("cannot encode png");
    std::fs::write(&out, png).expect("cannot write png");
    println!("wrote {}", out);
}
