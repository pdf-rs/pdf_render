
//! A page paints inside its own rectangle, and nowhere else.
//!
//! §14.11.2: the contents of a page are clipped to the crop box before the
//! first operator runs. A file that fills a rectangle far larger than the page
//! — a bleed, a ground drawn to the media box, a producer whose clip stack
//! ends in something bigger than the sheet — has to come out with clean paper
//! around the page, exactly as a viewer shows it.
//!
//! cargo test -p pdf_render --features vello_cpu --test page_clip

#![cfg(feature = "vello_cpu")]

use image::RgbaImage;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::vec2f;
use pdf::file::FileOptions;
use pdf_render::vello_cpu_backend::{OutlineBuilder, VelloCpuBackend};
use pdf_render::{page_bounds, render_page, Cache};

const MEDIA: f32 = 200.0;

/// A one-page PDF whose content stream is `content`, on a square media box.
fn pdf(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {MEDIA} {MEDIA}] \
             /Resources << >> /Contents 4 0 R >>"
        ),
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len() + 1, content),
    ];

    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for offset in offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

/// Render page 0 of `bytes` at 72 dpi into a target larger than the sheet, so
/// there is paper around the page to look at.
const PAD: u16 = 50;

fn render(bytes: Vec<u8>) -> RgbaImage {
    let file = FileOptions::cached()
        .load(bytes)
        .expect("the test PDF does not load");
    let page = file.get_page(0).expect("no page 0");
    let bounds = page_bounds(&page);
    // `page_bounds` is in millimetres and 72 dpi is one pixel per PDF unit,
    // which is what the page is measured in.
    let scale = 72.0 / 25.4;
    let width = (bounds.width() * scale).ceil() as u16;
    let height = (bounds.height() * scale).ceil() as u16;

    let mut cache = Cache::<OutlineBuilder>::new(OutlineBuilder::default());
    let mut backend = VelloCpuBackend::new(&mut cache, width + 2 * PAD, height + 2 * PAD);
    backend.fill_background(1.0, 1.0, 1.0);
    render_page(
        &mut backend,
        &file.resolver(),
        &page,
        Transform2F::from_translation(vec2f(PAD as f32, PAD as f32))
            * Transform2F::from_scale(vec2f(scale, scale)),
    )
    .expect("the page does not render");

    let png = backend.finish().into_png().expect("png");
    image::load_from_memory(&png).expect("decode").to_rgba8()
}

/// How far a pixel is from white, on its strongest channel.
fn ink(pixel: &image::Rgba<u8>) -> u8 {
    255 - pixel.0[0].min(pixel.0[1]).min(pixel.0[2])
}

#[test]
fn a_page_does_not_paint_outside_its_media_box() {
    // Red over everything, far past the sheet in both directions, and a black
    // square that is on the page.
    let content = "1 0 0 rg -1000 -1000 3000 3000 re f 0 0 0 rg 50 50 100 100 re f";
    let image = render(pdf(content));
    assert_paper_around_the_page(&image);

    // The page itself is painted, in the colours the content asks for.
    let red = image.get_pixel(PAD as u32 + 10, PAD as u32 + 10).0;
    assert!(red[0] > 240 && red[1] < 15 && red[2] < 15, "the ground is {red:?}");
    let black = image.get_pixel(image.width() / 2, image.height() / 2).0;
    assert!(black[0] < 15 && black[1] < 15 && black[2] < 15, "the square is {black:?}");
}

#[test]
fn a_clip_on_the_page_cannot_widen_the_page() {
    // A clip wider than the sheet, then red: the clip is legal, and it is
    // still the page that decides where the ink stops.
    let content = "-1000 -1000 3000 3000 re W n 1 0 0 rg -1000 -1000 3000 3000 re f";
    let image = render(pdf(content));
    assert_paper_around_the_page(&image);

    let red = image.get_pixel(PAD as u32 + 10, PAD as u32 + 10).0;
    assert!(red[0] > 240 && red[1] < 15 && red[2] < 15, "the ground is {red:?}");
}

/// The sheet was rendered `PAD` pixels in from the edge of the target: every
/// pixel outside it has to be the paper the backend started with.
fn assert_paper_around_the_page(image: &RgbaImage) {
    let (w, h) = (image.width(), image.height());
    for y in 0..h {
        for x in 0..w {
            let outside = x < PAD as u32
                || y < PAD as u32
                || x >= w - PAD as u32
                || y >= h - PAD as u32;
            if outside {
                let pixel = image.get_pixel(x, y);
                assert_eq!(ink(pixel), 0, "paint outside the page at {x},{y}: {pixel:?}");
            }
        }
    }
}
