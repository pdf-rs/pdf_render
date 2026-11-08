//! Put the SVG the viewer shows through an image comparator.
//!
//! usage: cargo run --features svg --example svg_cmp -- <svg | file.pdf page> <ref.png> <dpi> [out.png]
//!
//! The viewer draws SVG, so an ink comparison of *the CPU render* only ever
//! tells you about the CPU render. This example takes the bytes the browser
//! would get — either straight from the SVG backend, or an `.svg` file the
//! viewer already produced — rasterises them with resvg, and diffs the raster
//! against a reference PNG (poppler, pdf.js, or an earlier build).
//!
//! Coordinates are millimetres: the page size comes from the SVG's own
//! `viewBox`, so an SVG out of the browser needs no extra arguments.
//!
//! Nothing here is a pass/fail test. It prints where the difference *is*, in mm,
//! because "Eq. (11) looks wrong" is a coordinate, and a number of total ink
//! hides the sixteen pixels that matter.

use std::path::Path;

use image::RgbaImage;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::vec2f;
use pdf::file::FileOptions;
use pdf_render::svg_backend::{OutlineBuilder as SvgOutline, SvgBackend};
use pdf_render::{page_bounds, render_page, Cache};

/// Below this, a block's difference is antialiasing, not content.
const REPORT: f64 = 6.0;

fn main() {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        for local in ["fonts", "standard_fonts", "../standard_fonts"] {
            if Path::new(local).is_dir() {
                std::env::set_var("STANDARD_FONTS", local);
                break;
            }
        }
    }

    let args: Vec<String> = std::env::args().collect();
    let (svg, ref_path, dpi, out) = match args.len() {
        // svg_cmp file.pdf page ref.png dpi [out.png]
        6 | 7 if !args[2].ends_with(".svg") => {
            let pdf_path = &args[1];
            let page_nr: u32 = args[2].parse().expect("page must be a number");
            let svg = render_svg(pdf_path, page_nr);
            (svg, args[3].clone(), args[4].parse::<f32>().expect("dpi must be a number"), args.get(5).cloned())
        }
        // svg_cmp out.svg ref.png dpi [out.png]
        4 | 5 => {
            let text = std::fs::read_to_string(&args[1]).expect("cannot read the svg");
            (text, args[2].clone(), args[3].parse::<f32>().expect("dpi must be a number"), args.get(4).cloned())
        }
        _ => {
            eprintln!("usage: svg_cmp <svg | file.pdf page> <ref.png> <dpi> [out.png]");
            return;
        }
    };

    let (width_mm, height_mm) = view_box(&svg).expect("svg has no viewBox");
    let scale = dpi / 25.4;
    let width = (width_mm * scale).ceil().max(1.0) as u32;
    let height = (height_mm * scale).ceil().max(1.0) as u32;

    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(&svg, &opt).expect("cannot parse the svg");
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).expect("pixmap");
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let s = width as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
    let ours = match &out {
        Some(p) => {
            pixmap.save_png(Path::new(p)).expect("cannot save png");
            image::open(p).expect("cannot reopen png").to_rgba8()
        }
        None => RgbaImage::from_raw(width, height, pixmap.data().to_vec()).expect("pixmap size"),
    };
    let reference = image::open(&ref_path).expect("cannot open the reference").to_rgba8();

    println!("{width}x{height} px from a {width_mm:.1}x{height_mm:.1} mm page at {dpi} dpi");
    println!(
        "  ours {}x{}, reference {}x{}",
        ours.width(),
        ours.height(),
        reference.width(),
        reference.height()
    );

    let w = ours.width().min(reference.width()) as usize;
    let h = ours.height().min(reference.height()) as usize;
    // Darker is more ink: white is 0, black is 255.
    let ink = |im: &RgbaImage, x: usize, y: usize| -> f64 {
        let p = im.get_pixel(x as u32, y as u32);
        255.0 - (0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64)
    };

    // 16 px blocks, both sides, so a block that is *missing* is as visible as
    // one that moved.
    const B: usize = 16;
    let (bw, bh) = (w.div_ceil(B), h.div_ceil(B));
    let mut blocks = vec![(0.0f64, 0.0f64, 0.0f64); bw * bh];
    for y in 0..h {
        for x in 0..w {
            let (o, r) = (ink(&ours, x, y), ink(&reference, x, y));
            let b = &mut blocks[(y / B) * bw + x / B];
            b.0 += o;
            b.1 += r;
            b.2 += (o - r).abs();
        }
    }
    let total_ours: f64 = blocks.iter().map(|b| b.0).sum();
    let total_ref: f64 = blocks.iter().map(|b| b.1).sum();
    println!(
        "total ink: ours {total_ours:.0}, reference {total_ref:.0} ({:+.2}%)",
        (total_ours - total_ref) / total_ref * 100.0
    );

    let mm = (width_mm / w as f32, height_mm / h as f32);
    let mut worst: Vec<(f64, usize, usize, f64, f64)> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.2, (i % bw) * B, (i / bw) * B, b.0, b.1))
        .collect();
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let area = (B * B) as f64;
    println!("worst 16 px blocks (mm; ink ours vs reference):");
    let mut shown = 0;
    for (d, x, y, io, ir) in &worst {
        if d / area < REPORT || shown == 20 {
            break;
        }
        shown += 1;
        println!(
            "  x {:6.1} y {:6.1}  diff {:6.2}  ink ours {:6.1} ref {:6.1}",
            *x as f32 * mm.0,
            *y as f32 * mm.1,
            d / area,
            io / area,
            ir / area
        );
    }
    if shown == 0 {
        println!("  none above {REPORT}");
    }
}

/// Render one page through the SVG backend, exactly as the viewer does.
fn render_svg(path: &str, page_nr: u32) -> String {
    let file = FileOptions::cached().open(path).expect("cannot open the file");
    let page = file.get_page(page_nr - 1).expect("page index out of range");
    let bounds = page_bounds(&page);
    let mut cache = Cache::<SvgOutline>::new(SvgOutline {});
    let mut backend = SvgBackend::new(&mut cache);
    backend.set_paint_background(true);
    render_page(&mut backend, &file.resolver(), &page, Transform2F::from_scale(vec2f(1.0, 1.0)))
        .unwrap_or_else(|e| panic!("page {page_nr} failed: {e:?}"));
    let (glyphs, paths) = backend.counts();
    let page = backend.finish();
    println!(
        "page {page_nr}: {}x{} mm, {glyphs} glyph(s), {paths} path(s), {} layer(s)",
        bounds.width().round(),
        bounds.height().round(),
        page.layers.len()
    );
    page.single_svg()
        .expect("the page has more than one layer; use the viewer's own file instead")
        .to_string()
}

/// `viewBox="0 0 215.9 279.4"` — the page in millimetres (svg output is mm).
fn view_box(svg: &str) -> Option<(f32, f32)> {
    let i = svg.find("viewBox=\"")? + "viewBox=\"".len();
    let rest = &svg[i..];
    let end = rest.find('"')?;
    let mut it = rest[..end].split_whitespace().skip(2);
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}
