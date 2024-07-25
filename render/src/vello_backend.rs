use std::sync::Arc;

use font::{pathfinder_impl::PathBuilder, Encoder, Glyph};
use pathfinder_color::{ColorF, ColorU};
use pathfinder_content::{
    fill::FillRule,
    outline::{ContourIterFlags, Outline},
    pattern::Pattern,
    segment::{Segment, SegmentKind},
};
use pathfinder_geometry::{rect::RectF, transform2d::Transform2F, vector::Vector2F};
use vello::{
    glyph::skrifa::color::Brush,
    kurbo::{Affine, BezPath, Cap, Stroke},
    peniko::{Blob, BrushRef, Color, Fill, Format, Image, Mix},
    Scene,
};

use crate::{font::FontRc, load_image, Backend, Cache, DrawMode, FillMode};

pub struct VelloBackend<'a> {
    scene: Scene,
    clip_paths: Vec<(BezPath, FillRule)>,
    cache: &'a mut Cache<OutlineBuilder>,
    current_clip_path: Option<usize>,
}

impl<'a> VelloBackend<'a> {
    pub fn new(cache: &'a mut Cache<OutlineBuilder>) -> Self {
        VelloBackend {
            scene: Scene::new(),
            clip_paths: vec![],
            cache,
            current_clip_path: None,
        }
    }
    fn set_clip_path(&mut self, clip_path: Option<usize>) {
        if clip_path == self.current_clip_path {
            if let Some(_) = self.current_clip_path {
                self.scene.pop_layer();
            }
            if let Some(n) = clip_path {
                let (path, fill_rule) = &self.clip_paths[n];
                self.scene
                    .push_layer(Mix::Clip, 1.0, Affine::IDENTITY, path);
            }
            self.current_clip_path = clip_path;
        }
    }
    pub fn finish(self) -> Scene {
        self.scene
    }
}

pub fn outline_to_bez(outline: &Outline) -> BezPath {
    use vello::kurbo::Point;
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

fn convert_fill(fill: &FillMode) -> BrushRef<'static> {
    match fill.color {
        crate::Fill::Solid(r, g, b) => {
            let ColorU { r, g, b, a } = ColorF::new(r, g, b, fill.alpha).to_u8();
            BrushRef::Solid(Color { r, g, b, a })
        }
        _ => BrushRef::Solid(Color {
            r: 255,
            g: 0,
            b: 255,
            a: 127,
        }),
    }
}
fn convert_stroke(stroke: &crate::backend::Stroke) -> vello::kurbo::Stroke {
    let (dash_pattern, dash_offset) = stroke.dash_pattern.clone().unwrap_or_default();
    let end_cap = match stroke.style.line_cap {
        pathfinder_content::stroke::LineCap::Butt => Cap::Butt,
        pathfinder_content::stroke::LineCap::Square => Cap::Square,
        pathfinder_content::stroke::LineCap::Round => Cap::Round,
    };
    let join = match stroke.style.line_join {
        pathfinder_content::stroke::LineJoin::Bevel => vello::kurbo::Join::Bevel,
        pathfinder_content::stroke::LineJoin::Miter(_) => vello::kurbo::Join::Miter,
        pathfinder_content::stroke::LineJoin::Round => vello::kurbo::Join::Round,
    };
    vello::kurbo::Stroke {
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

impl<'a> Backend for VelloBackend<'a> {
    type Encoder = OutlineBuilder;
    type ClipPathId = usize;

    fn create_clip_path(
        &mut self,
        path: Outline,
        fill_rule: FillRule,
        parent: Option<Self::ClipPathId>,
    ) -> Self::ClipPathId {
        let id = self.clip_paths.len();
        self.clip_paths.push((outline_to_bez(&path), fill_rule));
        id
    }
    fn draw(
        &mut self,
        outline: &pathfinder_content::outline::Outline,
        mode: &crate::DrawMode,
        fill_rule: pathfinder_content::fill::FillRule,
        transform: pathfinder_geometry::transform2d::Transform2F,
        clip: Option<Self::ClipPathId>,
    ) {
        self.set_clip_path(clip);

        let transform = transform_to_affine(transform);
        if let Some(fill) = mode.fill() {
            let style = match fill_rule {
                FillRule::EvenOdd => Fill::EvenOdd,
                FillRule::Winding => Fill::NonZero,
            };
            let brush = convert_fill(fill);
            let shape = outline_to_bez(outline);
            self.scene.fill(style, transform, brush, None, &shape);
        }
        if let Some((stroke, stroke_mode)) = mode.stroke() {
            let style = match fill_rule {
                FillRule::EvenOdd => Fill::EvenOdd,
                FillRule::Winding => Fill::NonZero,
            };
            let brush = convert_fill(stroke);
            let stroke = convert_stroke(stroke_mode);
            let shape = outline_to_bez(outline);
            self.scene.stroke(&stroke, transform, brush, None, &shape);
        }
    }
    fn add_text(&mut self, span: crate::TextSpan<OutlineBuilder>, clip: Option<Self::ClipPathId>) {}

    fn set_view_box(&mut self, r: pathfinder_geometry::rect::RectF) {}

    fn draw_image(
        &mut self,
        xref: pdf::object::Ref<pdf::object::XObject>,
        im: &pdf::object::ImageXObject,
        resources: &pdf::object::Resources,
        transform: pathfinder_geometry::transform2d::Transform2F,
        mode: crate::BlendMode,
        clip: Option<Self::ClipPathId>,
        resolve: &impl pdf::object::Resolve,
    ) {
        if let Some((data, width, height)) = self
            .cache
            .get_image(xref, im, resources, resolve, mode)
            .rgba_data()
        {
            let image: Image = Image::new(Blob::new(data), Format::Rgba8, width, height);

            if let Some(clip_id) = clip {
                let (clip_path, _) = self.clip_paths.get(clip_id).unwrap();
        
                self.scene.push_layer(Mix::Clip, 1.0,  Affine::IDENTITY, clip_path);
            }
        
            let im_tr = transform
                * Transform2F::from_scale(Vector2F::new(1.0 / (width as f32), -1.0 / (height as f32)))
                * Transform2F::from_translation(Vector2F::new(0.0, -(height as f32)));
        
            self.scene
                .draw_image(&image, transform_to_affine(im_tr));
        
            if clip.is_some() {
                self.scene.pop_layer();
            }
        }
    }
    
    fn draw_inline_image(
        &mut self,
        im: &std::sync::Arc<pdf::object::ImageXObject>,
        resources: &pdf::object::Resources,
        transform: pathfinder_geometry::transform2d::Transform2F,
        mode: crate::BlendMode,
        clip: Option<Self::ClipPathId>,
        resolve: &impl pdf::object::Resolve,
    ) {
        if let Ok(image_data)  = load_image(im, resources, resolve, mode) {
            let width = image_data.width();
            let height = image_data.height();
            let image: Image = Image::new(Blob::new(image_data.rgba_data()), Format::Rgba8, width, height);

            if let Some(clip_id) = clip {
                let (clip_path, _) = self.clip_paths.get(clip_id).unwrap();
    
                self.scene.push_layer(Mix::Clip, 1.0,  Affine::IDENTITY, clip_path);
            }
    
            let im_tr = transform
                * Transform2F::from_scale(Vector2F::new(1.0 / (width as f32), -1.0 / (height as f32)))
                * Transform2F::from_translation(Vector2F::new(0.0, -(height as f32)));
    
            self.scene
                .draw_image(&image, transform_to_affine(im_tr));
    
            if clip.is_some() {
                self.scene.pop_layer();
            }
        }
    }
    fn get_font(
        &mut self,
        font_ref: &pdf::object::MaybeRef<pdf::font::Font>,
        resolve: &impl pdf::object::Resolve,
    ) -> Result<Option<std::sync::Arc<crate::FontEntry<OutlineBuilder>>>, pdf::PdfError> {
        self.cache.get_font(font_ref, resolve)
    }
    fn draw_glyph(
        &mut self,
        font: &FontRc<Self::Encoder>,
        glyph: &font::Glyph<Self::Encoder>,
        mode: &DrawMode,
        transform: pathfinder_geometry::transform2d::Transform2F,
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
                    match font.glyph(id) {
                        Some(Glyph {
                            shape: Shape::Simple(ref outline),
                            ..
                        }) => {
                            self.draw(outline, mode, FillRule::Winding, transform * tr, clip);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
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