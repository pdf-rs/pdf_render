use pdf::object::ColorSpace;

use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_content::{
    fill::FillRule,
    stroke::{StrokeStyle, OutlineStrokeToFill},
    outline::Outline,
};
use pathfinder_renderer::{
    scene::{DrawPath, ClipPath, ClipPathId, Scene},
    paint::{PaintId, Paint},
};
use pathfinder_color::ColorF;

#[derive(Clone)]
pub struct GraphicsState<'a> {
    pub transform: Transform2F,
    pub stroke_style: StrokeStyle,

    pub fill_color: ColorF,
    pub fill_paint: Option<PaintId>,
    pub stroke_color: ColorF,
    pub stroke_paint: Option<PaintId>,
    pub clip_path: Option<ClipPath>,
    pub clip_path_id: Option<ClipPathId>,
    pub fill_color_space: &'a ColorSpace,
    pub stroke_color_space: &'a ColorSpace,

    pub stroke_alpha: f32,
    pub fill_alpha: f32,
}

#[derive(Copy, Clone)]
pub enum DrawMode {
    Fill,
    Stroke,
    FillStroke,
}

impl<'a> GraphicsState<'a> {
    pub fn set_fill_color(&mut self, (r, g, b): (f32, f32, f32)) {
        if (r, g, b) != (self.fill_color.r(), self.fill_color.g(), self.fill_color.b()) {
            self.fill_color.set_r(r);
            self.fill_color.set_g(g);
            self.fill_color.set_b(b);
            self.fill_paint = None;
        }
    }
    pub fn set_fill_alpha(&mut self, alpha: f32) {
        let a = self.fill_alpha * alpha;
        if a != self.fill_color.a() {
            self.fill_color.set_a(a);
            self.fill_paint = None;
        }
    }
    pub fn set_stroke_color(&mut self, (r, g, b): (f32, f32, f32)) {
        if (r, g, b) != (self.fill_color.r(), self.stroke_color.g(), self.stroke_color.b()) {
            self.stroke_color.set_r(r);
            self.stroke_color.set_g(g);
            self.stroke_color.set_b(b);
            self.stroke_paint = None;
        }
    }
    pub fn set_stroke_alpha(&mut self, alpha: f32) {
        let a = self.stroke_alpha * alpha;
        if a != self.stroke_color.a() {
            self.stroke_color.set_a(a);
            self.stroke_paint = None;
        }
    }
    pub fn fill_paint(&mut self, scene: &mut Scene) -> PaintId {
        let color = self.fill_color;
        *self.fill_paint.get_or_insert_with(|| scene.push_paint(&Paint::from_color(color.to_u8())))
    }
    pub fn stroke_paint(&mut self, scene: &mut Scene) -> PaintId {
        let color = self.stroke_color;
        *self.stroke_paint.get_or_insert_with(|| scene.push_paint(&Paint::from_color(color.to_u8())))
    }
    pub fn merge_clip_path(&mut self, mut outline: Outline, fill_rule: FillRule) {
        if let Some(ref outer) = self.clip_path {
            let mut clipped_outline = Outline::new();
            for outer_contour in outer.outline().contours() {
                let clip_polygon = outer_contour.points();
                let mut clipped = outline.clone();
                clipped.clip_against_polygon(clip_polygon);
                clipped_outline.push_outline(clipped);
            }
            outline = clipped_outline;
        }
        
        let mut clip_path = ClipPath::new(outline);
        clip_path.set_fill_rule(fill_rule);
        self.clip_path = Some(clip_path);
    }
    pub fn clip_path_id(&mut self, scene: &mut Scene) -> Option<ClipPathId> {
        match (self.clip_path.as_ref(), self.clip_path_id) {
            (None, None) => None,
            (Some(_), Some(id)) => Some(id),
            (Some(clip_path), None) => {
                let id = scene.push_clip_path(clip_path.clone());
                self.clip_path_id = Some(id);
                Some(id)
            },
            _ => unreachable!()
        }
    }
    pub fn draw(&mut self, scene: &mut Scene, outline: &Outline, mode: DrawMode, fill_rule: FillRule) {
        self.draw_transform(scene, outline, mode, fill_rule, Transform2F::default());
    }
    fn fill(&mut self, scene: &mut Scene, outline: &Outline, fill_rule: FillRule, tr: Transform2F) {
        let mut draw_path = DrawPath::new(outline.clone().transformed(&tr), self.fill_paint(scene));
        draw_path.set_clip_path(self.clip_path_id(scene));
        draw_path.set_fill_rule(fill_rule);
        scene.push_draw_path(draw_path);
    }
    pub fn draw_transform(&mut self, scene: &mut Scene, outline: &Outline, mode: DrawMode, fill_rule: FillRule, transform: Transform2F) {
        let tr = self.transform * transform;

        if matches!(mode, DrawMode::Fill | DrawMode::FillStroke) {
            self.fill(scene, outline, fill_rule, tr);
        }
        if matches!(mode, DrawMode::Stroke | DrawMode::FillStroke) {
            let mut stroke = OutlineStrokeToFill::new(outline, self.stroke_style);
            stroke.offset();
            let mut draw_path = DrawPath::new(stroke.into_outline().transformed(&tr), self.stroke_paint(scene));
            draw_path.set_clip_path(self.clip_path_id(scene));
            draw_path.set_fill_rule(fill_rule);
            scene.push_draw_path(draw_path);
        }
    }
}
