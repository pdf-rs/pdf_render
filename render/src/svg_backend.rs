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
    fill::FillRule,
    outline::{ContourIterFlags, Outline},
    segment::SegmentKind,
};
use pathfinder_geometry::{rect::RectF, transform2d::Transform2F, vector::Vector2F};

use crate::backend::Stroke;
use crate::{BlendMode, Cache, DrawMode, FillMode, FontRc, TextSpan};

use pdf::error::PdfError;
use pdf::font::Font as PdfFont;
use pdf::object::{ImageXObject, MaybeRef, Ref, Resolve, Resources, XObject};

/// How far, in glyph units (1/2048 em for most fonts), the first corner of a
/// rectangular glyph contour is moved so the rasteriser stops treating the
/// contour as a pixel-snappable rectangle. See [`path_data`].
const GLYPH_NUDGE: f32 = 0.5;

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
    /// Millimetres, the render space: the same numbers are the `mm` size of the
    /// document.
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

/// A gradient that goes into `<defs>`: the element's attributes and stops,
/// with the `id` added when the layer is written (ids carry the layer's
/// namespace).
struct GradientDef {
    /// `linearGradient` or `radialGradient`.
    element: &'static str,
    attrs: String,
    stops: String,
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
    symbols: Vec<Arc<str>>,
    /// `d` -> symbol id, so a repeated glyph costs one `<use>`.
    symbol_ids: std::collections::HashMap<Arc<str>, u32>,
    /// Symbols the layer being built refers to.
    used_symbols: Vec<u32>,
    /// Gradient paints in `<defs>`, by id: the element without its `id`.
    gradients: Vec<GradientDef>,
    /// `Arc::as_ptr` of a [`crate::shading::Gradient`] -> id, so one shading
    /// fills many shapes with one definition.
    gradient_ids: std::collections::HashMap<(usize, [u32; 6]), u32>,
    /// Gradients the layer being built refers to.
    used_gradients: Vec<u32>,
    /// Scratch for the path data of a glyph, so a page of text does not
    /// allocate a string per glyph just to look it up.
    glyph_buf: String,
    /// Scratch for `matrix(…)`.
    matrix_buf: String,
    /// `fill`/`fill-opacity` attributes for the last paint used, and the paint
    /// they were built from: a page of text repeats one paint thousands of
    /// times, and `#rrggbb` formatting is not free either.
    fill_attr: String,
    fill_key: Option<[u32; 4]>,
    /// `id` namespace of the layer being built: `l0_`, `l1_`, … Layers are
    /// separate documents that a host may splice into one, and `id`s are
    /// document-global, so numbering them per layer keeps two layers apart
    /// without a rename pass over the finished text. `scope` goes in front of
    /// that for a host that puts several pages in one document — a viewer that
    /// scrolls: page 34's glyphs are `p34_l0_g17`, and no two pages can
    /// resolve each other's `<use href="#…">`.
    prefix: String,
    scope: String,
    /// Why an image was left out, for a host that wants to say so.
    image_failures: Vec<String>,
}

impl<'a> SvgBackend<'a> {
    pub fn new(cache: &'a mut Cache<OutlineBuilder>) -> Self {
        SvgBackend {
            cache,
            view_box: RectF::default(),
            body: String::new(),
            prefix: String::from("l0_"),
            scope: String::new(),
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
            gradients: Vec::new(),
            gradient_ids: std::collections::HashMap::new(),
            used_gradients: Vec::new(),
            glyph_buf: String::new(),
            matrix_buf: String::new(),
            fill_attr: String::new(),
            fill_key: None,
            image_failures: Vec::new(),
        }
    }

    /// PDF pages are white paper. Turn this off when the host paints its own
    /// background (dark reading mode, checkerboard for transparency).
    pub fn set_paint_background(&mut self, paint: bool) {
        self.paint_background = paint;
    }

    /// Put every `id` this page writes behind `scope` (`p34_`), for a host
    /// that has more than one page in a document at a time: a scrolling
    /// viewer shows them inline, and `id`s and `url(#…)` are document-global,
    /// so without this page 35 would draw page 34's glyphs.
    pub fn set_id_scope(&mut self, scope: impl Into<String>) {
        self.scope = scope.into();
        self.prefix = format!("{}l0_", self.scope);
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

    /// Why images were left out, in paint order. Empty for a page whose
    /// pictures all decoded.
    pub fn image_failures(&self) -> &[String] {
        &self.image_failures
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
        // The render space is millimetres (`SCALE` in lib.rs), so the intrinsic
        // size is in `mm`, not in unitless CSS pixels: a letter page comes out
        // 215.9×279.4mm = 816×1056 CSS px in a browser, the same size a PDF
        // viewer shows at 100%, and an `<img>`/`<object>` gets the same. Rendered
        // unitless it was 216 px wide — a quarter of the page, unreadable.
        svg.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"");
        fmt_f(w, &mut svg);
        svg.push_str("mm\" height=\"");
        fmt_f(h, &mut svg);
        svg.push_str("mm\" viewBox=\"");
        fmt_f(x, &mut svg);
        svg.push(' ');
        fmt_f(y, &mut svg);
        svg.push(' ');
        fmt_f(w, &mut svg);
        svg.push(' ');
        fmt_f(h, &mut svg);
        svg.push_str("\" shape-rendering=\"geometricPrecision\">");
        let prefix = &self.prefix;
        if !self.used_clips.is_empty()
            || !self.used_symbols.is_empty()
            || !self.used_gradients.is_empty()
        {
            svg.push_str("<defs>");
            for &id in &self.used_clips {
                let clip = &self.clips[id];
                let _ = write!(svg, "<clipPath id=\"{prefix}c{id}\" clipPathUnits=\"userSpaceOnUse\">");
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
                let _ = write!(svg, "<g id=\"{prefix}g{id}\"><path d=\"{}\"/></g>", self.symbols[id as usize]);
            }
            // Gradients after the glyphs, so a `<use>` never refers to an
            // element further down the document than itself.
            for &id in &self.used_gradients {
                let g = &self.gradients[id as usize];
                let _ = write!(
                    svg,
                    "<{el} id=\"{prefix}r{id}\" {attrs}>{stops}</{el}>",
                    el = g.element,
                    attrs = g.attrs,
                    stops = g.stops,
                );
            }
            svg.push_str("</defs>");
        }
        // The backend's coordinates are already y-down: the font matrix flips
        // glyphs, and the CTM does the same for paths, so the content goes in
        // as it is. A flip here would rotate every page by 180°.
        svg.push_str("<g>");
        svg.push_str(&self.body);
        svg.push_str("</g></svg>");
        self.layers.push(Layer::Vector(svg));
        self.prefix = format!("{}l{}_", self.scope, self.layers.len());
        self.body.clear();
        self.used_clips.clear();
        self.used_symbols.clear();
        self.used_gradients.clear();
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
            let _ = write!(self.body, "<g clip-path=\"url(#{p}c{id})\">", p = self.prefix);
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
        path_data(outline, transform, 0.0, &mut d);
        if d.is_empty() {
            return;
        }
        self.paths_emitted += 1;
        self.update_fill(fill, transform);
        let opened = self.open_clips(clip);
        let rule = if fill_rule == FillRule::EvenOdd { "evenodd" } else { "nonzero" };
        let _ = write!(self.body, "<path d=\"{d}\"{} fill-rule=\"{rule}\"/>", self.fill_attr);
        self.close_clips(opened);
    }

    /// Cache ` fill="…" fill-opacity="…"` for the paint in force. Text is one
    /// colour for page after page, so this is nearly always a cache hit.
    fn update_fill(&mut self, fill: &FillMode, transform: Transform2F) {
        let key = match &fill.color {
            crate::Fill::Solid(r, g, b) => Some([r.to_bits(), g.to_bits(), b.to_bits(), fill.alpha.to_bits()]),
            // Pattern fills are rasterised by some other path; flagged, not painted.
            crate::Fill::Pattern(_) => Some([u32::MAX, 0, 0, fill.alpha.to_bits()]),
            // A gradient is an object with an id, not a value: look it up.
            crate::Fill::Gradient(_) => None,
        };
        match key {
            Some(key) => {
                if self.fill_key == Some(key) {
                    return;
                }
                self.fill_key = Some(key);
            }
            // A solid fill after a gradient must not reuse the gradient's
            // attributes just because the key is back to `None`.
            None => self.fill_key = None,
        }
        let (paint, opacity) = match &fill.color {
            crate::Fill::Gradient(gradient) => {
                let id = self.gradient_def(gradient, transform);
                if !self.used_gradients.contains(&id) {
                    self.used_gradients.push(id);
                }
                (format!("url(#{}r{id})", self.prefix), fill.alpha)
            }
            _ => fill_attrs(fill),
        };
        self.fill_attr.clear();
        let _ = write!(self.fill_attr, " fill=\"{paint}\" fill-opacity=\"{opacity}\"");
    }

    /// The gradient's `<defs>` entry, and its id. Written once per layer that
    /// uses it: `id`s are document-global and carry the layer's namespace.
    fn gradient_def(&mut self, gradient: &Arc<crate::shading::Gradient>, transform: Transform2F) -> u32 {
        use crate::shading::GradientKind;

        // Shading space -> the coordinates the path data is in (the CTM is
        // baked into the path). Keyed by both, since one gradient drawn under
        // two transforms needs two `<defs>` entries.
        let matrix = transform * gradient.matrix;
        let key = (
            Arc::as_ptr(gradient) as usize,
            [
                matrix.m11().to_bits(),
                matrix.m21().to_bits(),
                matrix.m12().to_bits(),
                matrix.m22().to_bits(),
                matrix.m13().to_bits(),
                matrix.m23().to_bits(),
            ],
        );
        if let Some(&id) = self.gradient_ids.get(&key) {
            return id;
        }
        let mut attrs = String::from("gradientUnits=\"userSpaceOnUse\"");
        let element = match gradient.kind {
            GradientKind::Linear { start, end } => {
                attrs.push_str(" x1=\"");
                fmt_f(start.x(), &mut attrs);
                attrs.push_str("\" y1=\"");
                fmt_f(start.y(), &mut attrs);
                attrs.push_str("\" x2=\"");
                fmt_f(end.x(), &mut attrs);
                attrs.push_str("\" y2=\"");
                fmt_f(end.y(), &mut attrs);
                attrs.push('"');
                "linearGradient"
            }
            GradientKind::Radial { start, start_radius, end, end_radius } => {
                // SVG 2's `fr` is the start radius. Without it a type 3
                // shading with r0 > 0 would paint the inner disk with the
                // first stop, which is only right when r0 is 0.
                attrs.push_str(" cx=\"");
                fmt_f(end.x(), &mut attrs);
                attrs.push_str("\" cy=\"");
                fmt_f(end.y(), &mut attrs);
                attrs.push_str("\" r=\"");
                fmt_f(end_radius, &mut attrs);
                attrs.push_str("\" fx=\"");
                fmt_f(start.x(), &mut attrs);
                attrs.push_str("\" fy=\"");
                fmt_f(start.y(), &mut attrs);
                attrs.push_str("\" fr=\"");
                fmt_f(start_radius, &mut attrs);
                attrs.push('"');
                "radialGradient"
            }
        };
        // A shading always pads: an end that `/Extend` switched off carries a
        // transparent stop instead, which no `spreadMethod` can express.
        attrs.push_str(" spreadMethod=\"pad\"");
        attrs.push_str(" gradientTransform=\"matrix(");
        matrix_data(matrix, &mut attrs);
        attrs.push_str(")\"");

        let mut stops = String::new();
        for stop in &gradient.stops {
            let ColorU { r, g, b, .. } = ColorF::new(stop.color.0, stop.color.1, stop.color.2, 1.0).to_u8();
            stops.push_str("<stop offset=\"");
            fmt_f(stop.offset, &mut stops);
            let _ = write!(stops, "\" stop-color=\"#{r:02x}{g:02x}{b:02x}\"/>");
        }

        let id = self.gradients.len() as u32;
        self.gradients.push(GradientDef { element, attrs, stops });
        self.gradient_ids.insert(key, id);
        id
    }

    /// One occurrence of a glyph outline: register it in `<defs>` the first
    /// time, then a `<use>` that places and colours it.
    ///
    /// The glyph's path data is cached across pages by its address in the font
    /// cache (`font`); a page of text offers the same glyph dozens of times.
    fn emit_glyph(&mut self, font: usize, outline: &Outline, transform: Transform2F, fill: &FillMode, clip: Option<usize>) {
        let key = outline as *const Outline as usize;
        let d: Arc<str> = match self.cache.glyph_path(font, key) {
            Some(d) => d,
            None => {
                self.glyph_buf.clear();
                path_data(outline, Transform2F::default(), GLYPH_NUDGE, &mut self.glyph_buf);
                let d: Arc<str> = Arc::from(self.glyph_buf.as_str());
                self.cache.put_glyph_path(font, key, d.clone());
                d
            }
        };
        if d.is_empty() {
            return;
        }
        let id = match self.symbol_ids.get(&d) {
            Some(&id) => id,
            None => {
                let id = self.symbols.len() as u32;
                self.symbols.push(d.clone());
                self.symbol_ids.insert(d.clone(), id);
                id
            }
        };
        if !self.used_symbols.contains(&id) {
            self.used_symbols.push(id);
        }
        self.update_fill(fill, transform);
        self.matrix_buf.clear();
        matrix_data(transform, &mut self.matrix_buf);
        let opened = self.open_clips(clip);
        // `fill` inherits into the referenced <path>, which sets no paint of
        // its own.
        let _ = write!(
            self.body,
            "<use href=\"#{p}g{id}\" transform=\"matrix({})\"{} />",
            self.matrix_buf, self.fill_attr,
            p = self.prefix,
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
        path_data(&path, Transform2F::default(), 0.0, &mut d);
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
            if let Err(ref e) = *image.0 {
                self.image_failures.push(e.to_string());
            }
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
        let font_key = font.address();
        match glyph.shape {
            Shape::Empty => {}
            Shape::Simple(ref outline) => self.emit_glyph(font_key, outline, transform, fill, clip),
            Shape::Compound(ref parts) => {
                use font::Font;
                for &(id, tr) in parts.iter() {
                    if let Some(Glyph { shape: Shape::Simple(ref outline), .. }) = font.glyph(id) {
                        self.emit_glyph(font_key, outline, transform * tr, fill, clip);
                    }
                }
            }
        }
    }

    fn add_text(&mut self, _span: TextSpan<OutlineBuilder>, _clip: Option<usize>) {
        self.text_spans += 1;
    }
}

/// Stroke to fill, the way the other backends do it: dashes first, then the
/// outline stroker. See [`crate::stroke::stroke_to_fill`].
fn stroke_outline(outline: &Outline, stroke_mode: &Stroke) -> Outline {
    crate::stroke::stroke_to_fill(outline, stroke_mode)
}

/// The image's unit square onto the page, as an SVG matrix, in the page's own
/// coordinates — the ones the vector layers are written in, y down.
///
/// Two flips meet here and only one is wanted. PDF image space has its first
/// row of samples at unit y = 0 while user space has y up, which is why a
/// figure placed with a negative `d` is upright; the page the backend draws on
/// is y down, like SVG, so the row order has to be flipped once — `1 - v` —
/// and then no more. `combine` used to flip the layer a second time, which put
/// every figure on the page mirrored about its middle, and no test noticed
/// because the pixels never got there.
///
/// The matrix is derived from three points rather than from the fields of a
/// composed transform, because getting that composition wrong is silent: the
/// `<image>` keeps its right size and position and paints as a line, since one
/// column of the matrix comes out zero.
fn image_matrix(transform: Transform2F) -> [f32; 6] {
    let at = |u: f32, v: f32| transform * Vector2F::new(u, 1.0 - v);
    let origin = at(0.0, 0.0);
    let across = at(1.0, 0.0);
    let down = at(0.0, 1.0);
    // SVG's `matrix(a b c d e f)` is x' = a x + c y + e, y' = b x + d y + f.
    [
        across.x() - origin.x(),
        across.y() - origin.y(),
        down.x() - origin.x(),
        down.y() - origin.y(),
        origin.x(),
        origin.y(),
    ]
}

fn fill_attrs(fill: &FillMode) -> (String, f32) {
    match &fill.color {
        crate::Fill::Solid(r, g, b) => {
            let ColorU { r, g, b, .. } = ColorF::new(*r, *g, *b, 1.0).to_u8();
            (format!("#{r:02x}{g:02x}{b:02x}"), fill.alpha)
        }
        // Patterns are rasterised by some other path; mark them rather than
        // silently painting them black.
        crate::Fill::Pattern(_) => ("#ff0000".to_string(), fill.alpha * 0.5),
        // Never reached: `update_fill` writes `url(#…)` for a gradient. Red,
        // like a pattern, if it ever is.
        crate::Fill::Gradient(_) => ("#ff0000".to_string(), fill.alpha * 0.5),
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
///
/// `nudge` (in the outline's own units, so only glyph space wants it) moves
/// the first corner of every contour that is exactly an axis-aligned
/// rectangle. Gecko's SVG rasteriser takes such a contour for a rectangle and
/// snaps it to the pixel grid: on page 5 of the Glowpunk paper the `l` of
/// "Stihl" came out 3 px wide with two hard edges at a zoom where every other
/// stem — same 236 font units, but drawn with curves — measured 3.5 px and
/// antialiased to match (pdf.js in the same browser, filling the same
/// rectangle into a canvas, antialiased it too; `shape-rendering=
/// "geometricPrecision"` on the root changed nothing). Half a font unit of
/// slant on one corner takes the contour off the rect path of the rasteriser,
/// and at any zoom it is far below a device pixel.
fn path_data(outline: &Outline, t: Transform2F, nudge: f32, out: &mut String) {
    if nudge == 0.0 {
        return path_data_plain(outline, t, out);
    }
    let (a, b, c, d, e, f) = (t.m11(), t.m21(), t.m12(), t.m22(), t.m13(), t.m23());
    let mut at = |v: Vector2F, out: &mut String| {
        let x = a * v.x() + c * v.y() + e;
        let y = b * v.x() + d * v.y() + f;
        fmt_f(x, out);
        out.push(' ');
        fmt_f(y, out);
    };
    // One contour at a time, so the rectangle test can look at every point
    // before the first one is written.
    let mut elems: Vec<(SegmentKind, Vector2F, Vector2F, Vector2F)> = Vec::new();
    for contour in outline.contours() {
        elems.clear();
        let mut straight = true;
        for elem in contour.iter(ContourIterFlags::empty()) {
            match elem.kind {
                SegmentKind::None => {}
                SegmentKind::Line => elems.push((SegmentKind::Line, Vector2F::default(), Vector2F::default(), elem.baseline.to())),
                kind => {
                    straight = false;
                    elems.push((kind, elem.ctrl.from(), elem.ctrl.to(), elem.baseline.to()));
                }
            }
        }
        // A rectangle: the corners of a closed contour, all straight, each
        // coordinate one of two values. A rect may also arrive unclosed — the
        // bar of a compound glyph, `M54 0L187 0L187 554L54 554` — since a fill
        // closes it.
        let mut rect = false;
        // The closing edge may be listed explicitly or left to `Z`.
        if nudge != 0.0 && straight && (elems.len() == 3 || elems.len() == 4) {
            if let Some(first) = contour.first_position() {
                let pts = [first, elems[0].3, elems[1].3, elems[2].3];
                let x0 = pts[0].x();
                let y0 = pts[0].y();
                let x1 = pts.iter().map(|p| p.x()).find(|&x| (x - x0).abs() > 1e-3);
                let y1 = pts.iter().map(|p| p.y()).find(|&y| (y - y0).abs() > 1e-3);
                if let (Some(x1), Some(y1)) = (x1, y1) {
                    rect = pts.iter().all(|p| {
                        let on_x = (p.x() - x0).abs() <= 1e-3 || (p.x() - x1).abs() <= 1e-3;
                        let on_y = (p.y() - y0).abs() <= 1e-3 || (p.y() - y1).abs() <= 1e-3;
                        on_x && on_y
                    });
                }
            }
        }
        if let Some(mut first) = contour.first_position() {
            if rect {
                first += Vector2F::new(nudge, nudge);
            }
            out.push('M');
            at(first, out);
        }
        for (kind, ctrl_from, ctrl_to, to) in elems.drain(..) {
            match kind {
                SegmentKind::None => {}
                SegmentKind::Line => {
                    out.push('L');
                    at(to, out);
                }
                SegmentKind::Quadratic => {
                    out.push('Q');
                    at(ctrl_from, out);
                    out.push(' ');
                    at(to, out);
                }
                SegmentKind::Cubic => {
                    out.push('C');
                    at(ctrl_from, out);
                    out.push(' ');
                    at(ctrl_to, out);
                    out.push(' ');
                    at(to, out);
                }
            }
        }
        if contour.is_closed() {
            out.push('Z');
        }
    }
}

/// The same, with nothing to test for: one pass over the outline, no buffer.
/// This is the path every vector fill and stroke takes, and on a page of small
/// shapes it is the hot loop.
fn path_data_plain(outline: &Outline, t: Transform2F, out: &mut String) {
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

/// Append `v` with at most three decimals and no trailing zeros — seven
/// below one, because those are matrices, not coordinates.
///
/// `format!("{v:.3}")` is what this used to do, and it was most of the time a
/// page took to render: Rust's float formatting is an exact-decimal algorithm
/// with a big-integer fallback, and a text-heavy page formats a few hundred
/// thousand coordinates. Three decimals of a page coordinate fit an integer
/// easily, so this rounds to thousandths and writes digits directly — same
/// output, a small fraction of the work.
///
/// A glyph is placed by a matrix whose scale is about 0.002, and three
/// decimals turned that into 0.002: every glyph 3% too small and the page 7%
/// lighter than it should be. Below 1, seven decimals keep the scale.
fn fmt_f(v: f32, out: &mut String) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    let decimals: u32 = if v.abs() >= 1.0 { 3 } else { 7 };
    let div = 10u64.pow(decimals) as f64;
    // f32 -> f64 is exact, and the product then rounds to a fraction of a unit
    // correctly. Past 2^53 units the rounding is no longer exact; those values
    // are rounded to whole units instead of being worth a special case.
    let scaled = (v as f64 * div).round();
    if scaled.abs() >= 9_007_199_254_740_992.0 {
        write_whole(v as f64, out);
        return;
    }
    let n = scaled as i64;
    if n == 0 {
        out.push('0');
        return;
    }
    let (neg, n) = if n < 0 { (true, -n) } else { (false, n) };
    if neg {
        out.push('-');
    }
    let div = div as u64;
    write_u64((n / div as i64) as u64, out);
    let frac = (n % div as i64) as u64;
    if frac == 0 {
        return;
    }
    out.push('.');
    let start = out.len();
    for i in (0..decimals).rev() {
        out.push((b'0' + ((frac / 10u64.pow(i)) % 10) as u8) as char);
    }
    let mut end = out.len();
    while end > start && out.as_bytes()[end - 1] == b'0' {
        end -= 1;
    }
    out.truncate(end);
}

/// Decimal digits of a non-negative integer, most significant first.
fn write_u64(mut n: u64, out: &mut String) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.push_str(std::str::from_utf8(&buf[i..]).unwrap_or("0"));
}

/// The rare oversized value: whole units, still no float formatting.
fn write_whole(v: f64, out: &mut String) {
    if v.abs() >= 1.8e19 {
        out.push('0'); // beyond u64: nothing sane to say, and no page is there
        return;
    }
    let n = v.round();
    let (neg, n) = if n < 0.0 { (true, (-n) as u64) } else { (false, n as u64) };
    if neg {
        out.push('-');
    }
    write_u64(n, out);
}
