use pdf::object::ColorSpace;

use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_content::{
    fill::FillRule,
    stroke::{StrokeStyle},
    outline::Outline,
};
use pathfinder_renderer::{
    scene::{ClipPath, ClipPathId},
    paint::{PaintId},
};
use crate::{Fill, backend::Stroke};


#[derive(Clone)]
pub struct GraphicsState<'a> {
    pub transform: Transform2F,
    pub stroke_style: StrokeStyle,

    pub fill_color: Fill,
    pub fill_color_alpha: f32,
    pub fill_paint: Option<PaintId>,
    pub stroke_color: Fill,
    pub stroke_color_alpha: f32,
    pub stroke_paint: Option<PaintId>,
    pub clip_path: Option<ClipPath>,
    pub clip_path_id: Option<ClipPathId>,
    pub fill_color_space: &'a ColorSpace,
    pub stroke_color_space: &'a ColorSpace,
    pub dash_pattern: Option<(&'a [f32], f32)>,

    pub stroke_alpha: f32,
    pub fill_alpha: f32,
}


impl<'a> GraphicsState<'a> {
    pub fn set_fill_color(&mut self, fill: Fill) {
        if fill != self.fill_color {
            self.fill_color = fill;
            self.fill_paint = None;
        }
    }
    pub fn set_fill_alpha(&mut self, alpha: f32) {
        let a = self.fill_alpha * alpha;
        if a != self.fill_color_alpha {
            self.fill_color_alpha = a;
            self.fill_paint = None;
        }
    }
    pub fn set_stroke_color(&mut self, fill: Fill) {
        if fill != self.stroke_color {
            self.stroke_color = fill;
            self.stroke_paint = None;
        }
    }
    pub fn set_stroke_alpha(&mut self, alpha: f32) {
        let a = self.stroke_alpha * alpha;
        if a != self.stroke_color_alpha {
            self.stroke_alpha = a;
            self.stroke_paint = None;
        }
    }
    pub fn merge_clip_path(&mut self, outline: Outline, fill_rule: FillRule) {
        /*
        if let Some(ref outer) = self.clip_path {
            println!("path a: {:?}", outline);
            let mut clipped_outline = Outline::new();
            for outer_contour in outer.outline().contours() {
                println!("path b: {:?}", outer_contour);
                let clip_polygon = outer_contour.points();
                let mut clipped = outline.clone();
                clipped.clip_against_polygon(clip_polygon);
                clipped_outline.push_outline(clipped);
            }
            outline = clipped_outline;
        }
        */
        let mut clip_path = ClipPath::new(outline);
        clip_path.set_fill_rule(fill_rule);
        self.clip_path = Some(clip_path);
    }
    pub fn stroke(&self) -> Stroke {
        Stroke {
            style: self.stroke_style,
            dash_pattern: self.dash_pattern.map(|(a, p)| (a.into(), p))
        }
    }
}
