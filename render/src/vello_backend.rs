use pathfinder_content::{outline::{ContourIterFlags, Outline}, segment::{Segment, SegmentKind}};
use vello::kurbo::{Vec2 as Vector2F, Rect as RectF, Affine, BezPath};
use vello::peniko::{Fill as FillRule};
use vello::{glyph::skrifa::color::Brush, peniko::{BrushRef, Color, Mix}, Scene};
use crate::{backend::Backend, Cache, DrawMode, FillMode};

pub struct VelloBackend<'a> {
    scene: Scene,
    clip_paths: Vec<(BezPath, FillRule)>,
    cache: &'a mut Cache,
    current_clip_path: Option<usize>
}

impl<'a> VelloBackend<'a> {
    pub fn new(cache: &'a mut Cache) -> Self {
        VelloBackend {
            scene: Scene::new(),
            clip_paths: vec![],
            cache,
            current_clip_path: None
        }
    }
    fn set_clip_path(&mut self, clip_path: Option<usize>) {
        if clip_path == self.current_clip_path {
            if let Some(_) = self.current_clip_path {
                self.scene.pop_layer();
            }
            if let Some(n) = clip_path {
                let (path, style) = &self.clip_paths[n];
                self.scene.push_layer(Mix::Clip, 1.0, Affine::IDENTITY, path);
            }
            self.current_clip_path = clip_path;
        }
    }
    pub fn finish(self) -> Scene {
        self.scene
    }
}

fn outline_to_bez(outline: &Outline) -> BezPath {
    use vello::kurbo::Point;
    fn point(v: Vector2F) -> Point {
        Point::new(v.x, v.y)
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
                    bez.quad_to(
                        point(elem.ctrl.from()),
                        point(elem.baseline.to())
                    );
                }
                SegmentKind::Cubic => {
                    bez.curve_to(
                        point(elem.ctrl.from()),
                        point(elem.ctrl.to()),
                        point(elem.baseline.to())
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
            let color = Color::rgba(r as f64, g as f64, b as f64, fill.alpha as f64);
            BrushRef::Solid(color)
        }
        _ => BrushRef::Solid(Color { r: 255, g: 0, b: 255, a: 127 })
    }
}

impl<'a> Backend for VelloBackend<'a> {
    type ClipPathId = usize;
    fn create_clip_path(&mut self, path: Outline, style: FillRule, parent: Option<Self::ClipPathId>) -> Self::ClipPathId {
        let id = self.clip_paths.len();
        self.clip_paths.push((outline_to_bez(&path), style));
        id
    }
    fn draw(&mut self, outline: &pathfinder_content::outline::Outline, mode: &DrawMode, style: FillRule, transform: Affine, clip: Option<Self::ClipPathId>) {
        self.set_clip_path(clip);

        if let Some(fill) = mode.fill() {
            let brush = convert_fill(fill);
            let shape = outline_to_bez(outline);
            self.scene.fill(style, transform, brush, None, &shape);
        }
        if let Some((fillMode, stroke)) = mode.stroke() {
            let brush = convert_fill(fillMode);
            let shape = outline_to_bez(outline);
            self.scene.stroke(stroke, transform, brush, None, &shape);
        }
    }
    fn add_text(&mut self, span: crate::TextSpan, clip: Option<Self::ClipPathId>) {
    }

    fn set_view_box(&mut self, r: RectF) {

    }
    fn draw_image(&mut self, xref: pdf::object::Ref<pdf::object::XObject>, im: &pdf::object::ImageXObject, resources: &pdf::object::Resources, transform: Affine, mode: crate::BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl pdf::object::Resolve) {

    }
    fn draw_inline_image(&mut self, im: &std::sync::Arc<pdf::object::ImageXObject>, resources: &pdf::object::Resources, transform: Affine, mode: crate::BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl pdf::object::Resolve) {

    }
    fn get_font(&mut self, font_ref: &pdf::object::MaybeRef<pdf::font::Font>, resolve: &impl pdf::object::Resolve) -> Result<Option<std::sync::Arc<crate::FontEntry>>, pdf::PdfError> {
        self.cache.get_font(font_ref, resolve)
    }
}
