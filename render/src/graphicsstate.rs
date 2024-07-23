use pdf::object::ColorSpace;
use pathfinder_content::{fill::FillRule, stroke::StrokeStyle};
use vello::kurbo::BezPath;
use crate::{Fill, Backend, backend::Stroke};
use pathfinder_geometry::{
    transform2d::Transform2F,
    rect::RectF,
};

type ClipPath = (BezPath, FillRule);

pub struct GraphicsState<'a, B: Backend> {
    pub transform: Transform2F,
    pub stroke_style: StrokeStyle,

    pub fill_color: Fill,
    pub fill_color_alpha: f32,
    pub fill_paint: Option<usize>,
    pub stroke_color: Fill,
    pub stroke_color_alpha: f32,
    pub stroke_paint: Option<usize>,
    pub clip_path_id: Option<B::ClipPathId>,
    pub clip_path: Option<ClipPath>,
    pub clip_path_rect: Option<RectF>,
    pub fill_color_space: &'a ColorSpace,
    pub stroke_color_space: &'a ColorSpace,
    pub dash_pattern: Option<(&'a [f32], f32)>,

    pub stroke_alpha: f32,
    pub fill_alpha: f32,

    pub overprint_fill: bool,
    pub overprint_stroke: bool,
    pub overprint_mode: i32,
}

impl<'a, B: Backend> Clone for GraphicsState<'a, B> {
    fn clone(&self) -> Self {
        GraphicsState {
            clip_path: self.clip_path.clone(),
            .. *self
        }
    }
}


impl<'a, B: Backend> GraphicsState<'a, B> {
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
    pub fn stroke(&self) -> Stroke {
        Stroke {
            style: self.stroke_style,
            dash_pattern: self.dash_pattern.map(|(a, p)| (a.into(), p))
        }
    }
}
