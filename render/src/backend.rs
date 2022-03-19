use pathfinder_color::ColorF;
use pathfinder_geometry::{
    transform2d::Transform2F,
    rect::RectF,
};
use pathfinder_content::{
    fill::FillRule,
    stroke::{StrokeStyle},
    outline::Outline,
};
use pdf::object::{Ref, XObject, ImageXObject, Resolve};
use pdf::error::PdfError;
use font::Glyph;
use super::{FontEntry, TextSpan};
use pdf::font::Font as PdfFont;
use std::sync::Arc;

pub trait Backend {
    fn set_clip_path(&mut self, path: &Outline);
    fn draw(&mut self, outline: &Outline, mode: DrawMode, fill_rule: FillRule, transform: Transform2F);
    fn set_view_box(&mut self, r: RectF);
    fn draw_image(&mut self, xref: Ref<XObject>, im: &ImageXObject, transform: Transform2F, resolve: &impl Resolve);
    fn draw_glyph(&mut self, glyph: &Glyph, mode: DrawMode, transform: Transform2F) {
        self.draw(&glyph.path, mode, FillRule::Winding, transform);
    }
    fn get_font(&mut self, font_ref: Ref<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, PdfError>;
    fn add_text(&mut self, span: TextSpan);
}
#[derive(Copy, Clone)]
pub enum DrawMode {
    Fill(ColorF),
    Stroke(ColorF, StrokeStyle),
    FillStroke(ColorF, ColorF, StrokeStyle),
}
