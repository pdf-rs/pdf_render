use pathfinder_content::stroke::StrokeStyle;
use pdf::object::ColorSpace;

use pathfinder_geometry::{transform2d::Transform2F, rect::RectF};
use crate::{Fill, backend::Stroke, Backend};

pub struct GraphicsState<'a, B: Backend> {
    pub transform: Transform2F,
    pub stroke_style: StrokeStyle,

    pub fill_color: Fill,
    pub fill_color_alpha: f32,
    pub stroke_color: Fill,
    pub stroke_color_alpha: f32,
    pub clip_path_id: Option<B::ClipPathId>,
    //pub clip_path: Option<ClipPath>,
    pub clip_path_rect: Option<RectF>,
    pub fill_color_space: &'a ColorSpace,
    pub stroke_color_space: &'a ColorSpace,
    pub dash_pattern: Option<(Vec<f32>, f32)>,

    pub stroke_alpha: f32,
    pub fill_alpha: f32,

    pub overprint_fill: bool,
    pub overprint_stroke: bool,
    pub overprint_mode: i32,
}

impl<'a, B: Backend> Clone for GraphicsState<'a, B> {
    fn clone(&self) -> Self {
        GraphicsState {
            transform: self.transform,
            stroke_style: self.stroke_style,
            fill_color: self.fill_color,
            fill_color_alpha: self.fill_color_alpha,
            stroke_color: self.stroke_color,
            stroke_color_alpha: self.stroke_color_alpha,
            clip_path_id: self.clip_path_id,
            clip_path_rect: self.clip_path_rect,
            fill_color_space: self.fill_color_space,
            stroke_color_space: self.stroke_color_space,
            // no longer `Copy`: the dash pattern is owned so that operators can
            // be parsed and dropped one at a time
            dash_pattern: self.dash_pattern.clone(),
            stroke_alpha: self.stroke_alpha,
            fill_alpha: self.fill_alpha,
            overprint_fill: self.overprint_fill,
            overprint_stroke: self.overprint_stroke,
            overprint_mode: self.overprint_mode,
        }
    }
}


impl<'a, B: Backend> GraphicsState<'a, B> {
    pub fn set_fill_color(&mut self, fill: Fill) {
        if fill != self.fill_color {
            self.fill_color = fill;
        }
    }
    pub fn set_fill_alpha(&mut self, alpha: f32) {
        let a = self.fill_alpha * alpha;
        if a != self.fill_color_alpha {
            self.fill_color_alpha = a;
        }
    }
    pub fn set_stroke_color(&mut self, fill: Fill) {
        if fill != self.stroke_color {
            self.stroke_color = fill;
        }
    }
    pub fn set_stroke_alpha(&mut self, alpha: f32) {
        let a = self.stroke_alpha * alpha;
        if a != self.stroke_color_alpha {
            self.stroke_alpha = a;
        }
    }
    pub fn stroke(&self) -> Stroke {
        Stroke {
            style: self.stroke_style,
            dash_pattern: self.dash_pattern.clone()
        }
    }
}
