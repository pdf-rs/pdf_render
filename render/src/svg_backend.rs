//! SVG output backend.
//!
//! A page becomes a small stack of *layers*, bottom to top:
//!
//! * [`Layer::Vector`] — one SVG document (`<svg>` … `</svg>`), same `viewBox`
//!   for every layer of the page.
//! * [`Layer::Image`] — an `/Image` XObject or an inline image, as raw RGBA plus
//!   the six numbers of the matrix that places it.
//!
//! Raster data never enters the SVG: no `data:` URIs, no base64, and therefore
//! no 4/3 blow-up of the page in memory. A page without images is exactly one
//! string.
//!
//! What comes out of the PDF never becomes markup. Path data is numbers we
//! format ourselves, colours reduce to `#rrggbb` plus an opacity, and no PDF
//! string (font name, name object, `/Alt` text, …) is ever written into the
//! document. That matters because these documents are handed to a browser: an
//! SVG loaded through `<img>` cannot run script, but interpolating text into
//! markup is how that guarantee gets lost.
//!
//! Deliberately not done here: text. `add_text` only counts spans, so there is
//! no selection or copy. Glyphs are drawn as outlines, like the other backends
//! do; a separate text layer (plain data, drawn by the host) is the follow-up.

use std::fmt::Write as _;
use std::sync::Arc;

use font::{pathfinder_impl::PathBuilder, Encoder, Glyph};
use pathfinder_color::{ColorF, ColorU};
use pathfinder_content::{
    dash::OutlineDash,
    fill::FillRule,
    outline::{ContourIterFlags, Outline},
    segment::SegmentKind,
    stroke::OutlineStrokeToFill,
};
use pathfinder_geometry::{rect::RectF, transform2d::Transform2F, vector::Vector2F};

use crate::backend::Stroke;
use crate::{BlendMode, Cache, DrawMode, FillMode, FontRc, TextSpan};

use pdf::error::PdfError;
use pdf::font::Font as PdfFont;
use pdf::object::{ImageXObject, MaybeRef, Ref, Resolve, Resources, XObject};

/// Glyph outlines, as pathfinder outlines.
///
/// The same encoder the scene and vello backends use, kept here so the SVG
/// backend depends on neither of them.
#[derive(Default)]
pub struct OutlineBuilder {}

impl Encoder for OutlineBuilder {
    type Pen<'a> = PathBuilder;
    type GlyphRef = Outline;

    fn encode_shape<'f, O, E>(
        &mut self,
        mut f: impl for<'a> FnMut(&'a mut Self::Pen<'a>) -> Result<O, E> + 'f,
    ) -> Result<(O, Self::GlyphRef), E> {
        let mut builder = PathBuilder::new();
        let o = f(&mut builder)?;
        Ok((o, builder.finish()))
    }
}

impl Clone for OutlineBuilder {
    fn clone(&self) -> Self {
        OutlineBuilder {}
    }
}

/// One element of the page, in paint order.
pub enum Layer {
    /// A vector layer, as a complete `<svg>` document.
    Vector(String),
    /// A raster object.
    Image(ImageLayer),
}

pub struct ImageLayer {
    /// Straight RGBA8, `width * height * 4` bytes, first row is the top row.
    pub rgba: Arc<&'static [u8]>,
    pub width: u32,
    pub height: u32,
    /// `[a, b, c, d, e, f]`: the SVG matrix mapping the unit square of the
    /// image — (0,0) top left, (1,1) bottom right — onto the page.
    pub matrix: [f32; 6],
    /// Clip in force, as path data in page coordinates, `None` for unclipped.
    /// Numbers only; the host applies it with `Path2D` if it wants it.
    pub clip: Option<String>,
}

/// The result of rendering one page.
pub struct SvgPage {
    /// `[min_x, min_y, width, height]`, the `viewBox` of every vector layer.
    pub view_box: [f32; 4],
    pub layers: Vec<Layer>,
}

impl SvgPage {
    pub fn width(&self) -> f32 {
        self.view_box[2]
    }
    pub fn height(&self) -> f32 {
        self.view_box[3]
    }
    /// The page as one document, for the common case of a page without images.
    pub fn single_svg(&self) -> Option<&str> {
        match self.layers.as_slice() {
            [Layer::Vector(s)] => Some(s),
            _ => None,
        }
    }
}

struct ClipDef {
    /// Path data in device space; no further transform is applied.
    d: String,
    even_odd: bool,
    parent: Option<usize>,
}

pub struct SvgBackend<'a> {
    cache: &'a mut Cache<OutlineBuilder>,
    view_box: RectF,
    /// Body of the vector layer being built.
    body: String,
    /// Clip paths created for this page, by id.
    clips: Vec<ClipDef>,
    /// Clip paths referenced by the layer being built.
    used_clips: Vec<usize>,
    layers: Vec<Layer>,
    paint_background: bool,
    text_spans: usize,
    bg_done: bool,
    inline_images_skipped: usize,
    /// Glyphs offered by the renderer, for diagnostics.
    glyphs_seen: usize,
    /// Paths actually written, for diagnostics.
    paths_emitted: usize,
    /// Every distinct glyph outline on the page, in glyph space, by symbol id.
    symbols: Vec<String>,
    /// `d` -> symbol id, so a repeated glyph costs one `<use>`.
    symbol_ids: std::collections::HashMap<String, u32>,
    /// Symbols the layer being built refers to.
    used_symbols: Vec<u32>,
}

impl<'a> SvgBackend<'a> {
    pub fn new(cache: &'a mut Cache<OutlineBuilder>) -> Self {
        SvgBackend {
            cache,
            view_box: RectF::default(),
            body: String::new(),
            clips: Vec::new(),
            used_clips: Vec::new(),
            layers: Vec::new(),
            paint_background: true,
            text_spans: 0,
            bg_done: false,
            inline_images_skipped: 0,
            glyphs_seen: 0,
            paths_emitted: 0,
            symbols: Vec::new(),
            symbol_ids: std::collections::HashMap::new(),
            used_symbols: Vec::new(),
        }
    }

    /// PDF pages are white paper. Turn this off when the host paints its own
    /// background (dark reading mode, checkerboard for transparency).
    pub fn set_paint_background(&mut self, paint: bool) {
        self.paint_background = paint;
    }

    /// How many text spans the page had. They are not drawn; this only says
    /// whether a text layer would be non-empty.
    pub fn text_span_count(&self) -> usize {
        self.text_spans
    }

    /// How many inline images were skipped, see `draw_inline_image`.
    pub fn inline_images_skipped(&self) -> usize {
        self.inline_images_skipped
    }

    /// Glyphs the renderer offered and paths written, for diagnostics: a page
    /// whose text produced no paths is a bug, not an empty page.
    pub fn counts(&self) -> (usize, usize) {
        (self.glyphs_seen, self.paths_emitted)
    }

    pub fn finish(mut self) -> SvgPage {
        self.flush();
        SvgPage { view_box: self.view_box_array(), layers: self.layers }
    }

    fn view_box_array(&self) -> [f32; 4] {
        let r = self.view_box;
        [r.min_x(), r.min_y(), r.width(), r.height()]
    }

    /// Lay the white page under everything, once, in the first layer.
    fn paint_background_once(&mut self) {
        if self.bg_done {
            return;
        }
        self.bg_done = true;
        if self.paint_background {
            let [x, y, w, h] = self.view_box_array();
            self.body.push_str("<rect fill=\"#ffffff\" x=\"");
            fmt_f(x, &mut self.body);
            self.body.push_str("\" y=\"");
            fmt_f(y, &mut self.body);
            self.body.push_str("\" width=\"");
            fmt_f(w, &mut self.body);
            self.body.push_str("\" height=\"");
            fmt_f(h, &mut self.body);
            self.body.push_str("\"/>");
        }
    }

    /// Close the vector layer being built, if it has content.
    fn flush(&mut self) {
        if self.body.is_empty() {
            return;
        }
        let [x, y, w, h] = self.view_box_array();
        let mut svg = String::with_capacity(self.body.len() + 256);
        svg.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"");
        fmt_f(w, &mut svg);
        svg.push_str("\" height=\"");
        fmt_f(h, &mut svg);
        svg.push_str("\" viewBox=\"");
        fmt_f(x, &mut svg);
        svg.push(' ');
        fmt_f(y, &mut svg);
        svg.push(' ');
        fmt_f(w, &mut svg);
        svg.push(' ');
        fmt_f(h, &mut svg);
        svg.push_str("\" shape-rendering=\"geometricPrecision\">");
        if !self.used_clips.is_empty() || !self.used_symbols.is_empty() {
            svg.push_str("<defs>");
            for &id in &self.used_clips {
                let clip = &self.clips[id];
                let _ = write!(svg, "<clipPath id=\"c{id}\" clipPathUnits=\"userSpaceOnUse\">");
                let _ = write!(
                    svg,
                    "<path d=\"{}\" clip-rule=\"{}\"/></clipPath>",
                    clip.d,
                    if clip.even_odd { "evenodd" } else { "nonzero" }
                );
            }
            // Each distinct glyph outline once, in glyph space; a `<use>` per
            // occurrence carries the position and the colour.
            for &id in &self.used_symbols {
                let _ = write!(svg, "<g id=\"g{id}\"><path d=\"{}\"/></g>", self.symbols[id as usize]);
            }
            svg.push_str("</defs>");
        }
        // The backend's coordinates are already y-down: the font matrix flips
        // glyphs, and the CTM does the same for paths, so the content goes in
        // as it is. A flip here would rotate every page by 180°.
        svg.push_str("<g>");
        svg.push_str(&self.body);
        svg.push_str("</g></svg>");        self.layers.push(Layer::Vector(svg));
        self.body.clear();
        self.used_clips.clear();
        self.used_symbols.clear();
    }

    /// Wrap the next element in the clip chain of `clip`, outermost first, and
    /// return how many groups were opened.
    fn open_clips(&mut self, clip: Option<usize>) -> usize {
        let mut chain = Vec::new();
        let mut next = clip;
        while let Some(id) = next {
            chain.push(id);
            next = self.clips[id].parent;
        }
        let n = chain.len();
        for id in chain.into_iter().rev() {
            if !self.used_clips.contains(&id) {
                self.used_clips.push(id);
            }
            let _ = write!(self.body, "<g clip-path=\"url(#c{id})\">");
        }
        n
    }

    fn close_clips(&mut self, n: usize) {
        for _ in 0..n {
            self.body.push_str("</g>");
        }
    }

    fn emit_filled(&mut self, outline: &Outline, fill: &FillMode, fill_rule: FillRule, transform: Transform2F, clip: Option<usize>) {
        let mut d = String::new();
        path_data(outline, transform, &mut d);
        if d.is_empty() {
            return;
        }
        self.paths_emitted += 1;
        let (color, opacity) = fill_attrs(fill);
        let opened = self.open_clips(clip);
        let _ = write!(
            self.body,
            "<path d=\"{d}\" fill=\"{color}\" fill-opacity=\"{opacity}\" fill-rule=\"{}\"/>",
            if fill_rule == FillRule::EvenOdd { "evenodd" } else { "nonzero" },
        );
        self.close_clips(opened);
    }

    /// One occurrence of a glyph outline: register it in `<defs>` the first
    /// time, then a `<use>` that places and colours it.
    fn emit_glyph(&mut self, outline: &Outline, transform: Transform2F, fill: &FillMode, clip: Option<usize>) {
        let mut d = String::new();
        path_data(outline, Transform2F::default(), &mut d);
        if d.is_empty() {
            return;
        }
        let id = match self.symbol_ids.get(&d) {
            Some(&id) => id,
            None => {
                let id = self.symbols.len() as u32;
                self.symbols.push(d.clone());
                self.symbol_ids.insert(d, id);
                id
            }
        };
        if !self.used_symbols.contains(&id) {
            self.used_symbols.push(id);
        }
        let (color, opacity) = fill_attrs(fill);
        let opened = self.open_clips(clip);
        let mut m = String::new();
        matrix_data(transform, &mut m);
        // `fill` inherits into the referenced <path>, which sets no paint of
        // its own.
        let _ = write!(
            self.body,
            "<use href=\"#g{id}\" transform=\"matrix({m})\" fill=\"{color}\" fill-opacity=\"{opacity}\"/>",
        );
        self.close_clips(opened);
        self.paths_emitted += 1;
    }

    /// Clip in force, as path data in page coordinates.
    fn clip_path_data(&self, clip: Option<usize>) -> Option<String> {
        clip.map(|id| self.clips[id].d.clone())
    }

    /// A glyph as plain paths, for the cases `<use>` cannot express.
    fn draw_glyph_paths(&mut self, font: &FontRc<OutlineBuilder>, glyph: &Glyph<OutlineBuilder>, mode: &DrawMode, transform: Transform2F, clip: Option<usize>) {
        use crate::Backend as _;
        use font::Shape;
        match glyph.shape {
            Shape::Empty => {}
            Shape::Simple(ref outline) => self.draw(outline, mode, FillRule::Winding, transform, clip),
            Shape::Compound(ref parts) => {
                use font::Font;
                for &(id, tr) in parts.iter() {
                    if let Some(Glyph { shape: Shape::Simple(ref outline), .. }) = font.glyph(id) {
                        self.draw(outline, mode, FillRule::Winding, transform * tr, clip);
                    }
                }
            }
        }
    }
}

impl<'a> crate::Backend for SvgBackend<'a> {
    type Encoder = OutlineBuilder;
    type ClipPathId = usize;
    type Image = ();

    fn create_clip_path(&mut self, path: Outline, fill_rule: FillRule, parent: Option<usize>) -> usize {
        let mut d = String::new();
        path_data(&path, Transform2F::default(), &mut d);
        let id = self.clips.len();
        self.clips.push(ClipDef { d, even_odd: matches!(fill_rule, FillRule::EvenOdd), parent });
        id
    }

    fn set_view_box(&mut self, r: RectF) {
        self.view_box = r;
        self.paint_background_once();
    }

    fn draw(&mut self, outline: &Outline, mode: &DrawMode, fill_rule: FillRule, transform: Transform2F, clip: Option<usize>) {
        if let Some(fill) = mode.fill() {
            self.emit_filled(outline, fill, fill_rule, transform, clip);
        }
        if let Some((stroke, stroke_mode)) = mode.stroke() {
            // Stroke in user space, then transform — PDF line width is scaled
            // by the CTM, including the non-uniform part, which an SVG stroke
            // on already-transformed coordinates would not reproduce.
            let filled = stroke_outline(outline, stroke_mode);
            self.emit_filled(&filled, stroke, FillRule::Winding, transform, clip);
        }
    }

    fn draw_image(&mut self, xobject_ref: Ref<XObject>, im: &ImageXObject, resources: &Resources, transform: Transform2F, mode: BlendMode, clip: Option<usize>, resolve: &impl Resolve) {
        let image = self.cache.get_image(xobject_ref, im, resources, resolve, mode);
        let Some((rgba, width, height)) = image.rgba_data() else {
            return;
        };
        let clip = self.clip_path_data(clip);
        // The image is opaque to the layer stack: close the vectors below it,
        // put the image on top, let later vectors open a new layer above.
        self.flush();
        self.layers.push(Layer::Image(ImageLayer {
            rgba,
            width,
            height,
            matrix: image_matrix(transform),
            clip,
        }));
    }

    fn draw_inline_image(&mut self, _im: &Arc<ImageXObject>, _resources: &Resources, _transform: Transform2F, _mode: BlendMode, _clip: Option<usize>, _resolve: &impl Resolve) {
        // Inline images live inside the content stream, so they cannot be a
        // layer of their own: the vectors around them are in the same string.
        // Embedding them would put raster data back into the markup, which is
        // what this backend exists to avoid. Counted, not drawn; the CPI layer
        // can be added when a real document needs it.
        self.inline_images_skipped += 1;
    }

    fn get_font(&mut self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<crate::FontEntry<OutlineBuilder>>>, PdfError> {
        self.cache.get_font(font_ref, resolve)
    }

    fn draw_glyph(&mut self, font: &FontRc<Self::Encoder>, glyph: &Glyph<Self::Encoder>, mode: &DrawMode, transform: Transform2F, clip: Option<usize>) {
        use font::Shape;
        self.glyphs_seen += 1;
        if mode.stroke().is_some() {
            // Outlined text cannot take its paint from a `<use>`; draw it as
            // paths. Rare, and correctness beats compactness.
            return self.draw_glyph_paths(font, glyph, mode, transform, clip);
        }
        let Some(fill) = mode.fill() else { return };
        match glyph.shape {
            Shape::Empty => {}
            Shape::Simple(ref outline) => self.emit_glyph(outline, transform, fill, clip),
            Shape::Compound(ref parts) => {
                use font::Font;
                for &(id, tr) in parts.iter() {
                    if let Some(Glyph { shape: Shape::Simple(ref outline), .. }) = font.glyph(id) {
                        self.emit_glyph(outline, transform * tr, fill, clip);
                    }
                }
            }
        }
    }

    fn add_text(&mut self, _span: TextSpan<OutlineBuilder>, _clip: Option<usize>) {
        self.text_spans += 1;
    }
}

/// Stroke to fill, the way the other backends do it: dashes first.
fn stroke_outline(outline: &Outline, stroke_mode: &Stroke) -> Outline {
    match stroke_mode.dash_pattern {
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
    }
}

/// Unit square of the image (y down) onto the page (y up).
fn image_matrix(transform: Transform2F) -> [f32; 6] {
    let m = transform * Transform2F::row_major(1.0, 0.0, 0.0, -1.0, 0.0, 1.0);
    // SVG's `matrix(a b c d e f)` is column-major on the last row:
    // x' = a x + c y + e, y' = b x + d y + f.
    [m.m11(), m.m21(), m.m12(), m.m22(), m.m13(), m.m23()]
}

fn fill_attrs(fill: &FillMode) -> (String, f32) {
    match fill.color {
        crate::Fill::Solid(r, g, b) => {
            let ColorU { r, g, b, .. } = ColorF::new(r, g, b, 1.0).to_u8();
            (format!("#{r:02x}{g:02x}{b:02x}"), fill.alpha)
        }
        // Patterns are rasterised by some other path; mark them rather than
        // silently painting them black.
        crate::Fill::Pattern(_) => ("#ff0000".to_string(), fill.alpha * 0.5),
    }
}

/// `a b c d e f` of an SVG `matrix()`, in the order SVG wants them.
fn matrix_data(t: Transform2F, out: &mut String) {
    let (a, b, c, d, e, f) = (t.m11(), t.m21(), t.m12(), t.m22(), t.m13(), t.m23());
    for (i, v) in [a, b, c, d, e, f].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        fmt_f(*v, out);
    }
}

// `BlendMode` is misnamed: it carries the *overprint* flag, not a blend mode
// (`Overlay` = overprint off). It must not become `mix-blend-mode` — that made
// every normal fill `overlay`, painting black text invisible on white.

/// SVG path data for `outline`, with `t` applied.
fn path_data(outline: &Outline, t: Transform2F, out: &mut String) {
    let (a, b, c, d, e, f) = (t.m11(), t.m21(), t.m12(), t.m22(), t.m13(), t.m23());
    let mut pt = |v: Vector2F, out: &mut String| {
        let x = a * v.x() + c * v.y() + e;
        let y = b * v.x() + d * v.y() + f;
        fmt_f(x, out);
        out.push(' ');
        fmt_f(y, out);
    };
    for contour in outline.contours() {
        if let Some(first) = contour.first_position() {
            out.push('M');
            pt(first, out);
        }
        for elem in contour.iter(ContourIterFlags::empty()) {
            match elem.kind {
                SegmentKind::None => {}
                SegmentKind::Line => {
                    out.push('L');
                    pt(elem.baseline.to(), out);
                }
                SegmentKind::Quadratic => {
                    out.push('Q');
                    pt(elem.ctrl.from(), out);
                    out.push(' ');
                    pt(elem.baseline.to(), out);
                }
                SegmentKind::Cubic => {
                    out.push('C');
                    pt(elem.ctrl.from(), out);
                    out.push(' ');
                    pt(elem.ctrl.to(), out);
                    out.push(' ');
                    pt(elem.baseline.to(), out);
                }
            }
        }
        if contour.is_closed() {
            out.push('Z');
        }
    }
}

/// Append `v` with at most three decimals and no trailing zeros.
///
/// Cheaper and more predictable than `format!`: one `write!` into the target
/// string, then a trim. Three decimals is ~0.35 µm on a 1000 mm page.
fn fmt_f(v: f32, out: &mut String) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    let start = out.len();
    let _ = write!(out, "{v:.3}");
    let mut end = out.len();
    while end > start && out.as_bytes()[end - 1] == b'0' {
        end -= 1;
    }
    if end > start && out.as_bytes()[end - 1] == b'.' {
        end -= 1;
    }
    if end > start && out.as_bytes()[end - 1] == b'-' {
        end -= 1; // "-" alone would be invalid; leave "0"
        out.truncate(end);
        out.push('0');
        return;
    }
    out.truncate(end);
}
