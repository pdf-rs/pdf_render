use pathfinder_color::{ColorF, ColorU};
use pathfinder_content::{
    fill::FillRule,
    stroke::{OutlineStrokeToFill},
    outline::Outline,
    pattern::{Pattern},
    dash::OutlineDash,
};
use pathfinder_renderer::{
    scene::{DrawPath, ClipPath, ClipPathId, Scene},
    paint::{PaintId, Paint},
};
use pathfinder_geometry::{
    vector::{Vector2F},
    rect::RectF, transform2d::Transform2F,
};
use pdf::object::{Ref, XObject, ImageXObject, Resolve, Resources, MaybeRef};
use crate::backend;

use super::{FontEntry, TextSpan, DrawMode, Backend, Fill, Cache};
use pdf::font::Font as PdfFont;
use pdf::error::PdfError;
use std::sync::Arc;

pub struct SceneBackend<'a> {
    scene: Scene,
    cache: &'a mut Cache,
}
impl<'a> SceneBackend<'a> {
    pub fn new(cache: &'a mut Cache) -> Self {
        let scene = Scene::new();
        SceneBackend {
            scene,
            cache
        }
    }
    pub fn finish(self) -> Scene {
        self.scene
    }
    fn paint(&mut self, fill: Fill, alpha: f32) -> PaintId {
        let paint = match fill {
            Fill::Solid(r, g, b) => Paint::from_color(ColorF::new(r, g, b, alpha).to_u8()),
            Fill::Pattern(_) => {
                Paint::black()
            }
        };
        self.scene.push_paint(&paint)
    }
}
impl<'a> Backend for SceneBackend<'a> {
    type ClipPathId = ClipPathId;
    fn create_clip_path(&mut self, path: Outline, fill_rule: FillRule, parent: Option<Self::ClipPathId>) -> Self::ClipPathId {
        let mut clip = ClipPath::new(path);
        clip.set_fill_rule(fill_rule);
        clip.set_clip_path(parent);
        self.scene.push_clip_path(clip)
    }
    fn set_view_box(&mut self, view_box: RectF) {
        self.scene.set_view_box(view_box);

        let white = self.scene.push_paint(&Paint::from_color(ColorU::white()));
        self.scene.push_draw_path(DrawPath::new(Outline::from_rect(view_box), white));

    }
    fn draw(&mut self, outline: &Outline, mode: &DrawMode, fill_rule: FillRule, transform: Transform2F, clip: Option<ClipPathId>) {
        match mode {
            DrawMode::Fill { fill } | DrawMode::FillStroke {fill, .. } => {
                let paint = self.paint(fill.color, fill.alpha);
                let mut draw_path = DrawPath::new(outline.clone().transformed(&transform), paint);
                draw_path.set_clip_path(clip);
                draw_path.set_fill_rule(fill_rule);
                draw_path.set_blend_mode(blend_mode(fill.mode));
                self.scene.push_draw_path(draw_path);
            }
            _ => {}
        }
        match mode {
            DrawMode::Stroke { stroke, stroke_mode }| DrawMode::FillStroke { stroke, stroke_mode, .. } => {
                let paint = self.paint(stroke.color, stroke.alpha);
                let contour = match stroke_mode.dash_pattern {
                    Some((ref pat, phase)) => {
                        let dashed = OutlineDash::new(outline, &*pat, phase).into_outline();
                        let mut stroke = OutlineStrokeToFill::new(&dashed, stroke_mode.style);
                        stroke.offset();
                        stroke.into_outline()
                    }
                    None => {
                        let mut stroke = OutlineStrokeToFill::new(outline, stroke_mode.style);
                        stroke.offset();
                        stroke.into_outline()
                    }
                };
                let mut draw_path = DrawPath::new(contour.transformed(&transform), paint);
                draw_path.set_clip_path(clip);
                draw_path.set_fill_rule(fill_rule);

            draw_path.set_blend_mode(blend_mode(stroke.mode));
                self.scene.push_draw_path(draw_path);
            }
            _ => {}
        }
    }
    fn draw_image(&mut self, xobject_ref: Ref<XObject>, im: &ImageXObject, resources: &Resources, transform: Transform2F, mode: backend::BlendMode, clip: Option<ClipPathId>,  resolve: &impl Resolve) {
        if let Ok(ref image) = *self.cache.get_image(xobject_ref, im, resources, resolve, mode).0 {
            let size = image.size();
            let size_f = size.to_f32();
            let outline = Outline::from_rect(transform * RectF::new(Vector2F::default(), Vector2F::new(1.0, 1.0)));
            let im_tr = transform
                * Transform2F::from_scale(Vector2F::new(1.0 / size_f.x(), -1.0 / size_f.y()))
                * Transform2F::from_translation(Vector2F::new(0.0, -size_f.y()));

            let mut pattern = Pattern::from_image(image.clone());
            pattern.apply_transform(im_tr);
            let paint = Paint::from_pattern(pattern);
            let paint_id = self.scene.push_paint(&paint);
            let mut draw_path = DrawPath::new(outline, paint_id);
            draw_path.set_clip_path(clip);
            draw_path.set_blend_mode(blend_mode(mode));

            self.scene.push_draw_path(draw_path);
        }
    }
    fn draw_inline_image(&mut self, _im: &Arc<ImageXObject>, _resources: &Resources, _transform: Transform2F, mode: backend::BlendMode, clip: Option<ClipPathId>, _resolve: &impl Resolve) {

    }

    fn get_font(&mut self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, PdfError> {
        self.cache.get_font(font_ref, resolve)
    }
    fn add_text(&mut self, span: TextSpan, clip: Option<Self::ClipPathId>) {}
}

fn blend_mode(mode: backend::BlendMode) -> pathfinder_content::effects::BlendMode {
    match mode {
        crate::BlendMode::Darken => pathfinder_content::effects::BlendMode::Multiply,
        crate::BlendMode::Overlay => pathfinder_content::effects::BlendMode::Overlay,
    }
}