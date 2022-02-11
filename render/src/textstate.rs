use pathfinder_geometry::{
    vector::Vector2F,
    transform2d::Transform2F,
    rect::RectF,
};
use pathfinder_content::{
    fill::FillRule,
};
use font::GlyphId;
use super::{
    BBox,
    fontentry::{FontEntry, TextEncoding},
    graphicsstate::{GraphicsState},
    DrawMode,
    Backend,
    TextSpan,
};
use std::convert::TryInto;
use pdf::content::TextMode;
use std::rc::Rc;
use itertools::Either;


#[derive(Clone)]
pub struct TextState {
    pub text_matrix: Transform2F, // tracks current glyph
    pub line_matrix: Transform2F, // tracks current line
    pub char_space: f32, // Character spacing
    pub word_space: f32, // Word spacing
    pub horiz_scale: f32, // Horizontal scaling
    pub leading: f32, // Leading
    pub font_entry: Option<Rc<FontEntry>>, // Text font
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
    pub fn draw_text(&mut self, backend: &mut impl Backend, gs: &GraphicsState, data: &[u8]) {
        let e = match self.font_entry {
            Some(ref e) => e,
            None => {
                warn!("no font set");
                return;
            }
        };

        let codepoints = if e.is_cid {
            Either::Left(data.chunks_exact(2).map(|s| u16::from_be_bytes(s.try_into().unwrap())))
        } else {
            Either::Right(data.iter().map(|&b| b as u16))
        };

        let glyphs = codepoints.map(|cid| {
            let (gid, is_space) = match e.encoding {
                TextEncoding::CID => (Some(GlyphId(cid as u32)), false),
                TextEncoding::Cmap(ref cmap) => (cmap.get(&cid).cloned(), cid == 0x20),
            };
            (cid, gid, is_space)
        });

        let draw_mode = match self.mode {
            TextMode::Fill => DrawMode::Fill(gs.fill_color),
            TextMode::FillAndClip => DrawMode::Fill(gs.fill_color),
            TextMode::FillThenStroke => DrawMode::FillStroke(gs.fill_color, gs.stroke_color, gs.stroke_style),
            TextMode::Invisible => return,
            TextMode::Stroke => DrawMode::Stroke(gs.stroke_color, gs.stroke_style),
            TextMode::StrokeAndClip => DrawMode::Stroke(gs.stroke_color, gs.stroke_style),
        };
        let e = self.font_entry.as_ref().expect("no font");
        let mut bbox = BBox::empty();

        let mut text = String::new();
        let tm = self.text_matrix;
        let tr = Transform2F::row_major(
            self.horiz_scale * self.font_size, 0., 0.,
            0., self.font_size, self.rise
        ) * e.font.font_matrix();
        
        let mut total_width = 0.0;
        for (cid, gid, is_space) in glyphs {
            if let Some(part) = e.to_unicode.as_ref().and_then(|m| m.get(cid)) {
                text.push_str(part);
            } else {
                debug!("no unicode for cid={}", cid);
            }

            //debug!("cid {} -> gid {:?}", cid, gid);
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
                total_width += advance;
                continue;
            }
            if let Some(glyph) = glyph {
                let transform = gs.transform * self.text_matrix * tr;
                if glyph.path.len() != 0 {
                    bbox.add(gs.transform * transform * glyph.path.bounds());
                    backend.draw_glyph(&glyph, draw_mode, transform);
                }
            } else {
                debug!("no glyph for gid {:?}", gid);
            }
            let advance = self.char_space * self.horiz_scale + width;
            self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));
            total_width += advance;
        }

        if let Some(bbox) = bbox.rect() {
            let origin = tm.translation();
            let transform = gs.transform * tm * Transform2F::from_scale(Vector2F::new(1.0, -1.0));
            let p1 = gs.transform * origin;
            let p2 = p1 + Vector2F::new(total_width, self.font_size);

            backend.add_text(TextSpan {
                rect: RectF::from_points(p1.min(p2), p1.max(p2)),
                width: total_width,
                bbox,
                text,
                font: e.clone(),
                font_size: self.font_size,
                color: gs.fill_color.to_u8(),
                transform,
            });
        }
    }
    pub fn advance(&mut self, delta: f32) {
        //debug!("advance by {}", delta);
        let advance = delta * self.font_size * self.horiz_scale;
        self.text_matrix = self.text_matrix * Transform2F::from_translation(Vector2F::new(advance, 0.));
    }
}