//! Software (CPU) rendering backend, built on `vello_cpu`.
//!
//! The GPU backend in `vello_backend` (feature `vello`) records a `vello::Scene` and
//! needs a wgpu device to turn it into pixels.  `vello_cpu` rasterises on the CPU
//! instead and is used through an immediate-mode [`RenderContext`]: every drawing call
//! is rasterised into strips right away, and [`RenderContext::render`] composites them
//! into a [`Pixmap`].  That makes it usable without any GPU at all.
//!
//! Two API differences shape this file:
//!
//! * `vello_cpu` has no `draw_image`; an image is a *paint* that is sampled in
//!   image pixel space, and the geometry is an ordinary filled path.  So an
//!   image is drawn by filling the rect `(0, 0) .. (w, h)` with the transform
//!   that maps image pixels onto the device.
//! * There is no scene graph, so clipping has to be pushed around each individual
//!   draw with [`RenderContext::push_layer`] / [`RenderContext::pop_layer`].
//!
//! Enable with `--features vello_cpu`.  Rendering a page:
//!
//! ```ignore
//! let mut backend = VelloCpuBackend::new(&mut cache, width, height);
//! render_page(&mut backend, &file, &page, transform)?;
//! let pixmap = backend.finish();
//! let png = pixmap.into_png()?;
//! ```

use std::sync::Arc;

use font::{pathfinder_impl::PathBuilder, Encoder, Glyph};
use pathfinder_color::{ColorF, ColorU};
use pathfinder_content::{fill::FillRule, outline::Outline};
use pathfinder_geometry::{rect::RectF, transform2d::Transform2F, vector::Vector2F};
use vello_cpu::{
    color::Srgb,
    kurbo::{Affine, BezPath, Rect},
    peniko::color::AlphaColor,
    peniko::{Blob, ImageData, ImageFormat},
    Image, ImageSource, Pixmap, RenderContext, Resources,
};

use crate::{cache::ImageResult, Backend, Cache, DrawMode, FillMode, FontRc};

/// The CPU backend rasterises drawings immediately, so it has no image handles of
/// its own; `Image` is only here to satisfy the associated type.
pub struct VelloCpuBackend<'a> {
    ctx: RenderContext,
    clip_paths: Vec<(BezPath, FillRule)>,
    cache: &'a mut Cache<OutlineBuilder>,
}

impl<'a> VelloCpuBackend<'a> {
    /// `width` and `height` are the size of the target pixmap in pixels.
    pub fn new(cache: &'a mut Cache<OutlineBuilder>, width: u16, height: u16) -> Self {
        VelloCpuBackend {
            ctx: RenderContext::new(width, height),
            clip_paths: Vec::new(),
            cache,
        }
    }

    pub fn width(&self) -> u16 {
        self.ctx.width()
    }

    /// Paint the whole target a single colour before anything else is drawn, like the
    /// white page of a PDF viewer.  Without this the page background stays transparent.
    pub fn fill_background(&mut self, r: f32, g: f32, b: f32) {
        use vello_cpu::peniko::color::{AlphaColor, Srgb};
        let (width, height) = (self.ctx.width(), self.ctx.height());
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx
            .set_paint(AlphaColor::<Srgb>::from_rgba8(
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
                255,
            ));
        self.ctx
            .fill_rect(&Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
    }

    pub fn height(&self) -> u16 {
        self.ctx.height()
    }

    /// Rasterise everything that was drawn and return the pixels (premultiplied RGBA8).
    pub fn finish(mut self) -> Pixmap {
        let mut resources = Resources::new();
        let mut pixmap = Pixmap::new(self.ctx.width(), self.ctx.height());
        self.ctx.flush();
        self.ctx.render(&mut pixmap, &mut resources);
        pixmap
    }

    /// Push the clip path as a layer.  The clip path is stored in device space, so the
    /// caller has to make sure the current transform is the identity.
    fn push_clip(&mut self, clip: Option<usize>) {
        if let Some(id) = clip {
            if let Some((path, fill_rule)) = self.clip_paths.get(id) {
                self.ctx.set_transform(Affine::IDENTITY);
                self.ctx.set_fill_rule(convert_fill_rule(*fill_rule));
                self.ctx.push_clip_layer(path);
            }
        }
    }

    fn pop_clip(&mut self, clip: Option<usize>) {
        if let Some(id) = clip {
            if id < self.clip_paths.len() {
                self.ctx.pop_layer();
            }
        }
    }

    /// Draw `source` (in image pixel space) with `transform`, which maps the
    /// unit square onto the device.
    fn draw_image_source(
        &mut self,
        source: ImageSource,
        width: u32,
        height: u32,
        transform: Transform2F,
        clip: Option<usize>,
    ) {
        if width == 0 || height == 0 {
            return;
        }

        // Image space is the unit square.  Flip y so that row 0 of the image (top)
        // ends up at y = 1 of the unit square, like the GPU backend does.
        let image_transform = transform
            * Transform2F::from_scale(Vector2F::new(
                1.0 / (width as f32),
                -1.0 / (height as f32),
            ))
            * Transform2F::from_translation(Vector2F::new(0.0, -(height as f32)));

        let tf = transform_to_affine(image_transform);

        // `vello_cpu` samples image paints in image pixel space: the paint
        // transform is inverted and applied to device coordinates, so what it
        // wants is the pixel -> device map.  `image_transform` already is that
        // map (`transform * scale(1/w, -1/h) * translate(0, -h)` sends pixel
        // (x, y) to the unit square with y flipped, and then to the device), so
        // it can be used directly -- as the geometry transform, with the image
        // rect in pixel space and an identity paint transform, so that geometry
        // and paint agree and the transform is not applied twice.
        let pixel_to_device = tf;

        self.push_clip(clip);
        self.ctx.set_paint(Image {
            image: source,
            sampler: Default::default(),
        });
        self.ctx.set_transform(pixel_to_device);
        self.ctx.reset_paint_transform();
        self.ctx.fill_rect(&Rect::new(
            0.0,
            0.0,
            f64::from(width),
            f64::from(height),
        ));
        self.pop_clip(clip);
    }

    /// Draw an image that is already in peniko form (as the cache stores them).
    fn draw_image_brush(
        &mut self,
        brush: &vello_cpu::peniko::ImageBrush,
        transform: Transform2F,
        clip: Option<usize>,
    ) {
        let source = ImageSource::from_peniko_image_data(&brush.image);
        self.draw_image_source(
            source,
            brush.image.width,
            brush.image.height,
            transform,
            clip,
        );
    }
}

pub fn outline_to_bez(outline: &Outline) -> BezPath {
    use pathfinder_content::outline::ContourIterFlags;
    use pathfinder_content::segment::SegmentKind;
    use vello_cpu::kurbo::Point;

    fn point(v: Vector2F) -> Point {
        Point::new(v.x() as f64, v.y() as f64)
    }

    let mut bez = BezPath::new();
    for contour in outline.contours() {
        if let Some(p) = contour.first_position() {
            bez.move_to(point(p));
        }
        for elem in contour.iter(ContourIterFlags::empty()) {
            match elem.kind {
                SegmentKind::None => {}
                SegmentKind::Line => {
                    bez.line_to(point(elem.baseline.to()));
                }
                SegmentKind::Quadratic => {
                    bez.quad_to(point(elem.ctrl.from()), point(elem.baseline.to()));
                }
                SegmentKind::Cubic => {
                    bez.curve_to(
                        point(elem.ctrl.from()),
                        point(elem.ctrl.to()),
                        point(elem.baseline.to()),
                    );
                }
            }
        }
        if contour.is_closed() {
            bez.close_path();
        }
    }

    bez
}

fn convert_fill_rule(fill_rule: FillRule) -> vello_cpu::peniko::Fill {
    match fill_rule {
        FillRule::EvenOdd => vello_cpu::peniko::Fill::EvenOdd,
        FillRule::Winding => vello_cpu::peniko::Fill::NonZero,
    }
}

fn convert_fill(fill: &FillMode) -> AlphaColor<Srgb> {
    match fill.color {
        crate::Fill::Solid(r, g, b) => {
            let ColorU { r, g, b, a } = ColorF::new(r, g, b, fill.alpha).to_u8();
            AlphaColor::from_rgba8(r, g, b, a)
        }
        _ => AlphaColor::from_rgba8(255, 0, 0, 127),
    }
}

fn convert_stroke(stroke: &crate::backend::Stroke) -> vello_cpu::kurbo::Stroke {
    use vello_cpu::kurbo::{Cap, Join};

    let (dash_pattern, dash_offset) = stroke.dash_pattern.clone().unwrap_or_default();
    let end_cap = match stroke.style.line_cap {
        pathfinder_content::stroke::LineCap::Butt => Cap::Butt,
        pathfinder_content::stroke::LineCap::Square => Cap::Square,
        pathfinder_content::stroke::LineCap::Round => Cap::Round,
    };
    let join = match stroke.style.line_join {
        pathfinder_content::stroke::LineJoin::Bevel => Join::Bevel,
        pathfinder_content::stroke::LineJoin::Miter(_) => Join::Miter,
        pathfinder_content::stroke::LineJoin::Round => Join::Round,
    };
    vello_cpu::kurbo::Stroke {
        width: stroke.style.line_width as f64,
        dash_offset: dash_offset as f64,
        dash_pattern: dash_pattern.into_iter().map(|f| f as f64).collect(),
        end_cap,
        join,
        miter_limit: 1.0,
        start_cap: end_cap,
    }
}

#[derive(Default)]
pub struct OutlineBuilder {}

impl Encoder for OutlineBuilder {
    type Pen<'a> = PathBuilder;

    type GlyphRef = Outline;

    fn encode_shape<'f, O, E>(
        &mut self,
        mut f: impl for<'a> FnMut(&'a mut Self::Pen<'a>) -> Result<O, E> + 'f,
    ) -> Result<(O, Self::GlyphRef), E> {
        let mut builder = PathBuilder::new();
        let o = f(&mut builder)?;
        Ok((o, builder.finish()))
    }
}

impl Clone for OutlineBuilder {
    fn clone(&self) -> Self {
        OutlineBuilder {}
    }
}

impl<'a> Backend for VelloCpuBackend<'a> {
    type Encoder = OutlineBuilder;
    type ClipPathId = usize;
    type Image = ();

    fn create_clip_path(
        &mut self,
        path: Outline,
        fill_rule: FillRule,
        _parent: Option<Self::ClipPathId>,
    ) -> Self::ClipPathId {
        let id = self.clip_paths.len();
        self.clip_paths.push((outline_to_bez(&path), fill_rule));
        id
    }

    fn draw(
        &mut self,
        outline: &Outline,
        mode: &DrawMode,
        fill_rule: FillRule,
        transform: Transform2F,
        clip: Option<Self::ClipPathId>,
    ) {
        let shape = outline_to_bez(outline);
        let tf = transform_to_affine(transform);

        self.push_clip(clip);
        self.ctx.set_transform(tf);
        if let Some(fill) = mode.fill() {
            self.ctx.set_fill_rule(convert_fill_rule(fill_rule));
            self.ctx.set_paint(convert_fill(fill));
            self.ctx.fill_path(&shape);
        }
        if let Some((stroke, stroke_mode)) = mode.stroke() {
            self.ctx.set_fill_rule(convert_fill_rule(fill_rule));
            self.ctx.set_paint(convert_fill(stroke));
            self.ctx.set_stroke(convert_stroke(stroke_mode));
            self.ctx.stroke_path(&shape);
        }
        self.pop_clip(clip);
    }

    fn set_view_box(&mut self, _r: RectF) {}

    fn draw_image(
        &mut self,
        xref: pdf::object::Ref<pdf::object::XObject>,
        im: &pdf::object::ImageXObject,
        resources: &pdf::object::Resources,
        transform: Transform2F,
        mode: crate::BlendMode,
        clip: Option<Self::ClipPathId>,
        resolve: &impl pdf::object::Resolve,
    ) {
        let ImageResult(r) = self.cache.get_image(xref, im, resources, resolve, mode);
        if let Ok(image) = &*r {
            self.draw_image_brush(image, transform, clip);
        }
    }

    fn draw_inline_image(
        &mut self,
        im: &Arc<pdf::object::ImageXObject>,
        resources: &pdf::object::Resources,
        transform: Transform2F,
        mode: crate::BlendMode,
        clip: Option<Self::ClipPathId>,
        resolve: &impl pdf::object::Resolve,
    ) {
        if let Ok(image_data) = crate::load_image(im, resources, resolve, mode) {
            let width = image_data.width();
            let height = image_data.height();
            let data = Arc::new(image_data.rgba_data().to_vec());
            let data = ImageData {
                data: Blob::new(data),
                format: ImageFormat::Rgba8,
                alpha_type: vello_cpu::peniko::ImageAlphaType::Alpha,
                width,
                height,
            };
            let source = ImageSource::from_peniko_image_data(&data);
            self.draw_image_source(source, width, height, transform, clip);
        }
    }

    fn draw_glyph(
        &mut self,
        font: &FontRc<Self::Encoder>,
        glyph: &Glyph<Self::Encoder>,
        mode: &DrawMode,
        transform: Transform2F,
        clip: Option<Self::ClipPathId>,
    ) {
        use font::Shape;
        match glyph.shape {
            Shape::Empty => {}
            Shape::Simple(ref outline) => {
                self.draw(outline, mode, FillRule::Winding, transform, clip)
            }
            Shape::Compound(ref parts) => {
                for &(id, tr) in parts.iter() {
                    use font::Font;
                    if let Some(Glyph {
                        shape: Shape::Simple(ref outline),
                        ..
                    }) = font.glyph(id)
                    {
                        self.draw(outline, mode, FillRule::Winding, transform * tr, clip);
                    }
                }
            }
        }
    }

    fn get_font(
        &mut self,
        font_ref: &pdf::object::MaybeRef<pdf::font::Font>,
        resolve: &impl pdf::object::Resolve,
    ) -> Result<Option<Arc<crate::FontEntry<OutlineBuilder>>>, pdf::PdfError> {
        self.cache.get_font(font_ref, resolve)
    }

    fn add_text(&mut self, _span: crate::TextSpan<OutlineBuilder>, _clip: Option<Self::ClipPathId>) {}
}

fn transform_to_affine(transform: Transform2F) -> Affine {
    Affine::new([
        transform.m11() as f64,
        transform.m21() as f64,
        transform.m12() as f64,
        transform.m22() as f64,
        transform.m13() as f64,
        transform.m23() as f64,
    ])
}
