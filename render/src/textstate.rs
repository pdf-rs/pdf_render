use pathfinder_geometry::{
    vector::Vector2F,
    transform2d::Transform2F,
};
use font::GlyphId;
use super::{
    BBox,
    fontentry::{FontEntry, TextEncoding},
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
pub struct TextState {
    pub text_matrix: Transform2F, // tracks current glyph
    pub line_matrix: Transform2F, // tracks current line
    pub char_space: f32, // Character spacing
    pub word_space: f32, // Word spacing
    pub horiz_scale: f32, // Horizontal scaling
    pub leading: f32, // Leading
    pub font_entry: Option<Arc<FontEntry>>, // Text font
    pub font_size: f32, // Text font size
    pub mode: TextMode, // Text rendering mode
    pub rise: f32, // Text rise
    pub knockout: f32, //Text knockout
}
impl TextState {
    pub fn new() -> TextState {
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
    pub fn draw_text(&mut self, backend: &mut impl Backend, gs: &GraphicsState, data: &[u8], span: &mut Span) {
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

        let glyphs = codepoints.map(|cid| {
            match e.encoding {
                TextEncoding::CID(None) => {
                    let unicode = std::char::from_u32(cid as u32).map(|c| SmallString::from(c));
                    (cid, Some(GlyphId(cid as u32)), unicode)
                },
                TextEncoding::CID(Some(ref to_unicode)) => {
                    match to_unicode.get(&cid) {
                        Some(&(gid, ref unicode)) => (cid, gid, Some(unicode.clone())),
                        None => (cid, None, None)
                    }
                },
                TextEncoding::Cmap(ref cmap) => {
                    match cmap.get(&cid) {
                        Some(&(gid, ref unicode)) => (cid, Some(gid), unicode.clone()),
                        None => (cid, None, None)
                    }
                }
            }
        });

        let draw_mode = match self.mode {
            TextMode::Fill => Some(DrawMode::Fill(gs.fill_color, gs.fill_color_alpha)),
            TextMode::FillAndClip => Some(DrawMode::Fill(gs.fill_color, gs.fill_color_alpha)),
            TextMode::FillThenStroke => Some(DrawMode::FillStroke(
                gs.fill_color, gs.fill_color_alpha,
                gs.stroke_color, gs.stroke_color_alpha,
                gs.stroke()
            )),
            TextMode::Invisible => None,
            TextMode::Stroke => Some(DrawMode::Stroke(gs.stroke_color, gs.stroke_color_alpha, gs.stroke())),
            TextMode::StrokeAndClip => Some(DrawMode::Stroke(gs.stroke_color, gs.stroke_color_alpha, gs.stroke())),
        };
        let e = self.font_entry.as_ref().expect("no font");

        let tr = Transform2F::row_major(
            self.horiz_scale * self.font_size, 0., 0.,
            0., self.font_size, self.rise
        ) * e.font.font_matrix();
        
        for (cid, gid, unicode) in glyphs {
            let is_space = matches!(e.encoding, TextEncoding::Cmap(_)) && unicode.as_deref() == Some(" ");

            //debug!("cid {} -> gid {:?} {:?}", cid, gid, unicode);
            let gid = match gid {
                Some(gid) => gid,
                None => {
                    debug!("no glyph for cid {}", cid);
                    GlyphId(0)
                } // lets hope that works…
            };
            let glyph = e.font.glyph(gid);
            let width: f32 = e.widths.as_ref().map(|w| w.get(cid as usize) * 0.001 * self.horiz_scale * self.font_size)
                .or_else(|| glyph.as_ref().map(|g| tr.m11() * g.metrics.advance))
                .unwrap_or(0.0);
            
            if is_space {
                let advance = self.word_space * self.horiz_scale + width;
                self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));
                span.width += advance;
                span.text.push(' ');
                continue;
            }
            if let Some(glyph) = glyph {
                let transform = gs.transform * self.text_matrix * tr;
                if glyph.path.len() != 0 {
                    span.bbox.add(gs.transform * transform * glyph.path.bounds());
                    if let Some(ref draw_mode) = draw_mode {
                        backend.draw_glyph(&glyph, draw_mode, transform);
                    }
                }
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