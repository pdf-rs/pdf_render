use pathfinder_geometry::{
    vector::Vector2F,
    transform2d::Transform2F,
};
use font::{Encoder, GlyphId, Shape};
use crate::{BlendMode, backend::{FillMode, Stroke}};

use super::{
    BBox,
    fontentry::{FontEntry},
    graphicsstate::{GraphicsState},
    DrawMode,
    Backend,
    TextChar,
};
use std::convert::TryInto;
use pdf::content::TextMode;
use std::sync::Arc;
use itertools::Either;
use istring::SmallString;

#[derive(Clone)]
pub struct TextState<E: Encoder + Clone + 'static> {
    pub text_matrix: Transform2F, // tracks current glyph
    pub line_matrix: Transform2F, // tracks current line
    pub char_space: f32, // Character spacing
    pub word_space: f32, // Word spacing
    pub horiz_scale: f32, // Horizontal scaling
    pub leading: f32, // Leading
    pub font_entry: Option<Arc<FontEntry<E>>>, // Text font
    pub font_size: f32, // Text font size
    pub mode: TextMode, // Text rendering mode
    pub rise: f32, // Text rise
    pub knockout: f32, //Text knockout
}
impl<E: Encoder + Clone + 'static> TextState<E> {
    pub fn new() -> TextState<E> {
        TextState {
            text_matrix: Transform2F::default(),
            line_matrix: Transform2F::default(),
            char_space: 0.,
            word_space: 0.,
            horiz_scale: 1.,
            leading: 0.,
            font_entry: None,
            font_size: 0.,
            mode: TextMode::Fill,
            rise: 0.,
            knockout: 0.
        }
    }
    pub fn reset_matrix(&mut self) {
        self.set_matrix(Transform2F::default());
    }
    pub fn translate(&mut self, v: Vector2F) {
        let m = self.line_matrix * Transform2F::from_translation(v);
        self.set_matrix(m);
    }
    
    // move to the next line
    pub fn next_line(&mut self) {
        self.translate(Vector2F::new(0., -self.leading));
    }
    // set text and line matrix
    pub fn set_matrix(&mut self, m: Transform2F) {
        self.text_matrix = m;
        self.line_matrix = m;
    }
    pub fn draw_text<B: Backend<Encoder = E>>(&mut self, backend: &mut B, gs: &GraphicsState<B>, data: &[u8], span: &mut Span, fill_mode: BlendMode, stroke_mode: BlendMode) {
        use font::Font;
        let e = match self.font_entry {
            Some(ref e) => e,
            None => {
                debug!("no font set");
                return;
            }
        };

        let codepoints = if e.is_cid {
            Either::Left(data.chunks_exact(2).map(|s| u16::from_be_bytes(s.try_into().unwrap())))
        } else {
            Either::Right(data.iter().map(|&b| b as u16))
        };

        let glyphs = codepoints.map(|cid|
            (cid, e.cmap.get(&cid).map(|&(gid, ref uni)| (gid, uni.clone())))
        );

        let fill = FillMode { color: gs.fill_color, alpha: gs.fill_color_alpha, mode: fill_mode };
        let stroke = FillMode { color: gs.stroke_color, alpha: gs.stroke_color_alpha, mode: stroke_mode };
        let stroke_mode = gs.stroke();

        let draw_mode = match self.mode {
            TextMode::Fill => Some(DrawMode::Fill { fill }),
            TextMode::FillAndClip => Some(DrawMode::Fill { fill }),
            TextMode::FillThenStroke => Some(DrawMode::FillStroke { fill, stroke, stroke_mode }),
            TextMode::Invisible => None,
            TextMode::Stroke => Some(DrawMode::Stroke { stroke, stroke_mode }),
            TextMode::StrokeAndClip => Some(DrawMode::Stroke { stroke, stroke_mode }),
        };
        let e = self.font_entry.as_ref().expect("no font");

          let tr = Transform2F::row_major(
            self.horiz_scale * self.font_size, 0., 0.,
            0., self.font_size, self.rise
        ) * e.font.font_matrix();
        
        for (cid, t) in glyphs {
            let (gid, unicode, is_space) = match t {
                Some((gid, unicode)) => {
                    let is_space = !e.is_cid && unicode.as_deref() == Some(" ");
                    (gid, unicode, is_space)
                }
                None => (GlyphId(0), None, cid == 0x20)
            };
            //debug!("cid {} -> gid {:?} {:?}", cid, gid, unicode);
            
            let glyph = e.font.glyph(gid);
            let width: f32 = e.widths.as_ref().map(|w| w.get(cid as usize) * 0.001 * self.horiz_scale * self.font_size)
                .or_else(|| glyph.as_ref().map(|g| tr.m11() * g.metrics.advance))
                .unwrap_or(0.0);
            
            if is_space {
                let advance = (self.char_space + self.word_space) * self.horiz_scale + width;
                self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));

                let offset = span.text.len();
                span.text.push(' ');
                span.chars.push(TextChar {
                    offset,
                    pos: span.width,
                    width
                });
                span.width += advance;
                continue;
            }
            if let (Some(glyph), Some(draw_mode)) = (glyph, draw_mode.as_ref()){
                let transform = gs.transform * self.text_matrix * tr;
                backend.draw_glyph(&e.font, &glyph, draw_mode, transform, gs.clip_path_id);
            } else {
                debug!("no glyph for gid {:?}", gid);
            }
            let advance = self.char_space * self.horiz_scale + width;
            self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));
            
            let offset = span.text.len();
            if let Some(s) = unicode {
                span.text.push_str(&*s);
                span.chars.push(TextChar {
                    offset,
                    pos: span.width,
                    width
                });
            }
            span.width += advance;
        }
    }
    pub fn advance(&mut self, delta: f32) -> f32 {
        //debug!("advance by {}", delta);
        let advance = delta * self.font_size * self.horiz_scale;
        self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));
        advance
    }
}

#[derive(Default)]
pub struct Span {
    pub text: String,
    pub chars: Vec<TextChar>,
    pub width: f32,
    pub bbox: BBox,
}