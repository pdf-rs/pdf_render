//! vello_cpu smoke tests: find out which combination of path/transform works.
//!
//! Each variant paints magenta and reports how many pixels came out non-transparent.

use vello_cpu::color::palette::css;
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::{Pixmap, RenderContext, Resources};

fn run(name: &str, width: u16, height: u16, setup: impl FnOnce(&mut RenderContext)) {
    let mut ctx = RenderContext::new(width, height);
    let mut resources = Resources::new();
    ctx.set_paint(css::MAGENTA);
    setup(&mut ctx);

    let mut target = Pixmap::new(width, height);
    ctx.flush();
    ctx.render(&mut target, &mut resources);

    let magenta = css::MAGENTA.premultiply().to_rgba8();
    let (mr, mg, mb, ma) = (magenta.r, magenta.g, magenta.b, magenta.a);
    let opaque = target.data().iter().filter(|px| px.a != 0).count();
    let full = target
        .data()
        .iter()
        .filter(|px| px.r == mr && px.g == mg && px.b == mb && px.a == ma)
        .count();
    println!(
        "{name:38} -> {opaque:5} non-transparent of {:5}, {full:5} exact magenta",
        target.data().len()
    );
}

fn main() {
    // a) baseline: fill_rect, identity transform.
    run("a) fill_rect identity", 100, 100, |ctx| {
        ctx.fill_rect(&Rect::new(20., 20., 80., 80.));
    });

    // b) fill_path of a rectangle, identity transform.
    run("b) fill_path identity", 100, 100, |ctx| {
        let mut path = vello_cpu::kurbo::BezPath::new();
        path.move_to((20., 20.));
        path.line_to((80., 20.));
        path.line_to((80., 80.));
        path.line_to((20., 80.));
        path.close_path();
        ctx.fill_path(&path);
    });

    // c) fill_path with the kind of transform the PDF backend uses: tiny scale,
    //    big source coordinates, no y flip.
    run("c) fill_path tiny scale", 216, 280, |ctx| {
        ctx.set_transform(Affine::new([
            0.001_808_675_1,
            0.0,
            0.0,
            -0.001_808_675_1,
            20.04,
            23.99,
        ]));
        let mut path = vello_cpu::kurbo::BezPath::new();
        path.move_to((10_000., 10_000.));
        path.line_to((30_000., 10_000.));
        path.line_to((30_000., 30_000.));
        path.line_to((10_000., 30_000.));
        path.close_path();
        ctx.fill_path(&path);
    });

    // d) same, but with a positive m22 (no mirroring): y grows downwards.
    run("d) fill_path tiny scale, no flip", 216, 280, |ctx| {
        ctx.set_transform(Affine::new([
            0.001_808_675_1,
            0.0,
            0.0,
            0.001_808_675_1,
            20.04,
            23.99,
        ]));
        let mut path = vello_cpu::kurbo::BezPath::new();
        path.move_to((5_000., 5_000.));
        path.line_to((25_000., 5_000.));
        path.line_to((25_000., 25_000.));
        path.line_to((5_000., 25_000.));
        path.close_path();
        ctx.fill_path(&path);
    });

    // e) fill_rect under the same mirroring transform.
    run("e) fill_rect tiny scale, mirrored", 216, 280, |ctx| {
        ctx.set_transform(Affine::new([
            0.001_808_675_1,
            0.0,
            0.0,
            -0.001_808_675_1,
            20.04,
            23.99,
        ]));
        ctx.fill_rect(&Rect::new(10_000., 10_000., 30_000., 30_000.));
    });

    // f) many small glyph-sized paths scattered over the page.
    run("f) 15 tiny glyph paths", 216, 280, |ctx| {
        for i in 0..15u32 {
            ctx.set_transform(Affine::new([
                0.001_808_675_1,
                0.0,
                0.0,
                -0.001_808_675_1,
                20.04 + f64::from(i) * 10.0,
                60.99,
            ]));
            let mut path = vello_cpu::kurbo::BezPath::new();
            path.move_to((0., 0.));
            path.line_to((4_000., 0.));
            path.line_to((4_000., 4_000.));
            path.line_to((0., 4_000.));
            path.close_path();
            ctx.fill_path(&path);
        }
    });
}
