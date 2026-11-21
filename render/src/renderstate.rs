use std::sync::Arc;

use log::{debug, info, warn};
use pathfinder_content::outline::ContourIterFlags;
use pdf::{object::*, t, try_opt};
use pdf::primitive::{Primitive, Dictionary, Name};
use pdf::content::{Op, Matrix, Point, Rect, Color, Rgb, Cmyk, Winding, FormXObject};
use pdf::error::{PdfError, Result};
use pdf::content::TextDrawAdjusted;
use crate::backend::{Backend, BlendMode, Stroke, FillMode};

use pathfinder_geometry::{
    vector::Vector2F,
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_content::{
    fill::FillRule,
    stroke::{LineCap, LineJoin, StrokeStyle},
    outline::{Outline, Contour},
};
use super::{
    graphicsstate::GraphicsState,
    textstate::{TextState, Span},
    DrawMode,
    TextSpan,
    Fill,
};
use crate::shading::{Extend, Gradient, GradientKind, Stop};

trait Cvt {
    type Out;
    fn cvt(self) -> Self::Out;
}
impl Cvt for Point {
    type Out = Vector2F;
    fn cvt(self) -> Self::Out {
        Vector2F::new(self.x, self.y)
    }
}
impl Cvt for Matrix {
    type Out = Transform2F;
    fn cvt(self) -> Self::Out {
        let Matrix { a, b, c, d, e, f } = self;
        Transform2F::row_major(a, c, e, b, d, f)
    }
}
impl Cvt for Rect {
    type Out = RectF;
    fn cvt(self) -> Self::Out {
        RectF::new(
            Vector2F::new(self.x, self.y),
            Vector2F::new(self.width, self.height)
        )
    }
}
impl Cvt for Winding {
    type Out = FillRule;
    fn cvt(self) -> Self::Out {
        match self {
            Winding::NonZero => FillRule::Winding,
            Winding::EvenOdd => FillRule::EvenOdd
        }
    }
}
impl Cvt for Rgb {
    type Out = (f32, f32, f32);
    fn cvt(self) -> Self::Out {
        let Rgb { red, green, blue } = self;
        (red, green, blue)
    }
}
impl Cvt for Cmyk {
    type Out = (f32, f32, f32, f32);
    fn cvt(self) -> Self::Out {
        let Cmyk { cyan, magenta, yellow, key } = self;
        (cyan, magenta, yellow, key)
    }
}

pub struct RenderState<'a, R: Resolve, B: Backend> {
    graphics_state: GraphicsState<'a, B>,
    text_state: TextState<B::Encoder>,
    stack: Vec<(GraphicsState<'a, B>, TextState<B::Encoder>)>,
    current_outline: Outline,
    current_contour: Contour,
    resolve: &'a R,
    resources: &'a Resources,
    backend: &'a mut B,
    /// The page's area in the backend's own coordinates. A `sh` paints "the
    /// region currently clipped", and a clip cannot always be expressed as a
    /// rectangle, so the shading is drawn over the whole page and the backend
    /// clips it — which needs the page rectangle here, not in the backend.
    device_rect: RectF,
    _debug: bool,
}

impl<'a, R: Resolve, B: Backend> RenderState<'a, R, B> {
    pub fn new(backend: &'a mut B, resolve: &'a R, resources: &'a Resources, root_transformation: Transform2F) -> Self {
        let graphics_state = GraphicsState {
            transform: root_transformation,
            fill_color: Fill::black(),
            fill_color_alpha: 1.0,
            fill_alpha: 1.0,
            stroke_color: Fill::black(),
            stroke_color_alpha: 1.0,
            stroke_alpha: 1.0,
            clip_path_id: None,
            //clip_path: None,
            clip_path_rect: None,
            fill_color_space: &ColorSpace::DeviceRGB,
            stroke_color_space: &ColorSpace::DeviceRGB,
            stroke_style: StrokeStyle {
                line_cap: LineCap::Butt,
                line_join: LineJoin::Miter(1.0),
                line_width: 1.0,
            },
            dash_pattern: None,
            overprint_fill: false,
            overprint_stroke: false,
            overprint_mode: 0,
        };
        let text_state = TextState::new();
        let stack = vec![];
        let current_outline = Outline::new();
        let current_contour = Contour::new();

        RenderState {
            graphics_state,
            text_state,
            stack,
            current_outline,
            current_contour,
            resources,
            resolve,
            backend,
            device_rect: RectF::default(),
            _debug: false,
        }
    }
    fn draw(&mut self, mode: &DrawMode, fill_rule: FillRule) {
        self.flush();
        self.backend.draw(&self.current_outline, mode, fill_rule, self.graphics_state.transform, self.graphics_state.clip_path_id);
        self.current_outline.clear();
    }
    #[allow(unused_variables)]
    pub fn draw_op(&mut self, op: &Op, op_nr: usize) -> Result<()> {
        self.backend.inspect_op(op_nr, op);
        match *op {
            Op::BeginMarkedContent { .. } => {}
            Op::EndMarkedContent { .. } => {}
            Op::MarkedContentPoint { .. } => {}
            // Type 3 glyph metrics; they set widths declared by d0/d1 and draw nothing.
            Op::GlyphWidth { .. } => {}
            Op::GlyphWidthAndBBox { .. } => {}
            Op::Close => {
                self.current_contour.close();
            }
            Op::MoveTo { p } => {
                self.flush();
                self.current_contour.push_endpoint(p.cvt());
            },
            Op::LineTo { p } => {
                self.current_contour.push_endpoint(p.cvt());
            },
            Op::CurveTo { c1, c2, p } => {
                self.current_contour.push_cubic(c1.cvt(), c2.cvt(), p.cvt());
            },
            Op::Rect { rect } => {
                self.flush();
                self.current_outline.push_contour(Contour::from_rect(rect.cvt()));
            },
            Op::EndPath => {
                self.current_contour.clear();
                self.current_outline.clear();
            }
            Op::Stroke => {
                self.draw(&DrawMode::Stroke {
                    stroke: FillMode {
                        color: self.graphics_state.stroke_color.clone(),
                        alpha: self.graphics_state.stroke_color_alpha,
                        mode: self.blend_mode_stroke(),
                    },
                    stroke_mode: self.graphics_state.stroke()},
                    FillRule::Winding
                );
            },
            Op::FillAndStroke { winding } => {
                self.draw(&DrawMode::FillStroke {
                    fill: FillMode {
                        color: self.graphics_state.fill_color.clone(),
                        alpha: self.graphics_state.fill_color_alpha,
                        mode: self.blend_mode_fill(),
                    },
                    stroke: FillMode {
                        color: self.graphics_state.stroke_color.clone(),
                        alpha: self.graphics_state.stroke_color_alpha,
                        mode: self.blend_mode_stroke()
                    },
                    stroke_mode: self.graphics_state.stroke()
                }, winding.cvt());
            }
            Op::Fill { winding } => {
                self.draw(&DrawMode::Fill {
                    fill: FillMode {
                        color: self.graphics_state.fill_color.clone(),
                        alpha: self.graphics_state.fill_color_alpha,
                        mode: self.blend_mode_fill(),
                    },
            }, winding.cvt());
            }
            Op::Shade { ref name } => self.draw_shading(name)?,
            Op::Clip { winding } => {
                self.flush();
                let mut path: Outline = self.current_outline.clone().transformed(&self.graphics_state.transform);
                let clip_path_rect = to_rect(&path);

                // A new clip never replaces the one in force: it narrows it.
                // Every branch therefore keeps `parent`, so the backend can
                // push the whole chain (see `push_clip`). Dropping the parent
                // — which this did whenever the new clip was a rectangle —
                // leaves only the last link, and pages whose innermost clip is
                // the full MediaBox get painted to the edge: that is the dark
                // sheet over Grundfosliterature-6154082.pdf page 15.
                let (path, r, parent) = match (self.graphics_state.clip_path_rect, clip_path_rect, self.graphics_state.clip_path_id) {
                    (Some(r1), Some(r2), Some(p)) => {
                        let r = r1.intersection(r2).unwrap_or_default();
                        (Outline::from_rect(r), Some(r), Some(p))
                    }
                    (Some(r), None, Some(p)) => {
                        path.clip_against_polygon(&[r.origin(), r.upper_right(), r.lower_right(), r.lower_left()]);
                        (path, None, Some(p))
                    }
                    (None, Some(_r), Some(p)) => {
                        (path, None, Some(p))
                    }
                    (None, Some(r), None) => {
                        (path, Some(r), None)
                    }
                    (None, None, Some(p)) => (path, None, Some(p)),
                    (None, None, None) => (path, None, None),
                    _ => unreachable!()
                };

                let id = self.backend.create_clip_path(path.clone(), winding.cvt(), parent);
                self.graphics_state.clip_path_id = Some(id);
                //self.graphics_state.clip_path = Some(clip);
                self.graphics_state.clip_path_rect = r;
            },

            Op::Save => {
                self.stack.push((self.graphics_state.clone(), self.text_state.clone()));
            },
            Op::Restore => {
                let (g, t) = self.stack.pop().ok_or_else(|| pdf::error::PdfError::Other { msg: "graphcs stack is empty".into() })?;
                self.graphics_state = g;
                self.text_state = t;
            },

            Op::Transform { matrix } => {
                self.graphics_state.transform = self.graphics_state.transform * matrix.cvt();
            }
            Op::LineWidth { width } => self.graphics_state.stroke_style.line_width = width,
            // `[] 0 d` is how a content stream says "solid" again: an empty
            // array is not a dash pattern. Stored as `None`, because a backend
            // that dashes with an empty pattern draws nothing at all — that is
            // how the fraction bars of nasa.pdf page 35 went missing.
            Op::Dash { ref pattern, phase } => {
                self.graphics_state.dash_pattern = if pattern.iter().any(|&v| v > 0.0) {
                    Some((pattern.clone(), phase))
                } else {
                    None
                }
            }
            Op::LineJoin { join } => {},
            Op::LineCap { cap } => {},
            Op::MiterLimit { limit } => {},
            Op::Flatness { tolerance } => {},
            Op::GraphicsState { ref name } => {
                let gs = try_opt!(self.resources.graphics_states.get(name));
                debug!("GS: {gs:?}");
                if let Some(lw) = gs.line_width {
                    self.graphics_state.stroke_style.line_width = lw;
                }
                self.graphics_state.set_fill_alpha(gs.fill_alpha.unwrap_or(1.0));
                self.graphics_state.set_stroke_alpha(gs.stroke_alpha.unwrap_or(1.0));

                if let Some((font_ref, size)) = gs.font {
                    // Same tolerance as `TextFont`: a font that cannot be read
                    // leaves its text unset, it does not fail the page.
                    let font = match self.resolve.get(font_ref) {
                        Ok(font) => self
                            .backend
                            .get_font(&MaybeRef::Indirect(font), self.resolve)
                            .unwrap_or_else(|e| {
                                warn!("gs font: {e:?}");
                                None
                            }),
                        Err(e) => {
                            warn!("gs font: {e:?}");
                            None
                        }
                    };
                    if let Some(e) = font {
                        debug!("new font: {} at size {}", e.name, size);
                        self.text_state.font_entry = Some(e);
                        self.text_state.font_size = size;
                    } else {
                        self.text_state.font_entry = None;
                    }
                }
                if let Some(op) = gs.overprint {
                    self.graphics_state.overprint_fill = op;
                    self.graphics_state.overprint_stroke = op;
                }
                if let Some(op) = gs.overprint_fill {
                    self.graphics_state.overprint_fill = op;
                }
                if let Some(m) = gs.overprint_mode {
                    self.graphics_state.overprint_mode = m;
                }
            },
            Op::StrokeColor { ref color } => {
                let mode = self.blend_mode_stroke();
                let color = t!(convert_color(&mut self.graphics_state.stroke_color_space, color, &self.resources, self.resolve, mode));
                let color = self.paint_of_pattern(color)?;
                self.graphics_state.set_stroke_color(color);
            },
            Op::FillColor { ref color } => {
                let mode = self.blend_mode_fill();
                let color = t!(convert_color(&mut self.graphics_state.fill_color_space, color, &self.resources, self.resolve, mode));
                let color = self.paint_of_pattern(color)?;
                self.graphics_state.set_fill_color(color);
            },
            Op::FillColorSpace { ref name } => {
                self.graphics_state.fill_color_space = self.color_space(name)?;
                self.graphics_state.set_fill_color(Fill::black());
            },
            Op::StrokeColorSpace { ref name } => {
                self.graphics_state.stroke_color_space = self.color_space(name)?;
                self.graphics_state.set_stroke_color(Fill::black());
            },
            Op::RenderingIntent { intent } => {},
            Op::BeginText => self.text_state.reset_matrix(),
            Op::EndText => {},
            Op::CharSpacing { char_space } => self.text_state.char_space = char_space,
            Op::WordSpacing { word_space } => self.text_state.word_space = word_space,
            Op::TextScaling { horiz_scale } => self.text_state.horiz_scale = 0.01 * horiz_scale,
            Op::Leading { leading } => self.text_state.leading = leading,
            Op::TextFont { ref name, size } => {
                let font = match self.resources.fonts.get(name) {
                    Some(font_ref) => match font_ref.load(self.resolve) {
                        Ok(font) => match self.backend.get_font(&font, self.resolve) {
                            Ok(font) => font,
                            Err(e) => {
                                warn!("font {name}: {e:?}");
                                None
                            }
                        },
                        Err(e) => {
                            // A font that cannot be read — a `Null` where the
                            // font program or its descriptor should be, which
                            // arXiv's writer leaves behind — costs the text set
                            // in that font, not the rest of the page. Failing
                            // the op used to fail the whole page with it.
                            warn!("font {name}: {e:?}");
                            None
                        }
                    },
                    None => None,
                };
                if let Some(e) = font {
                    debug!("new font: {} (is_cid={:?})", e.name, e.is_cid);
                    self.text_state.font_entry = Some(e);
                    self.text_state.font_size = size;
                } else {
                    info!("no font {}", name);
                    self.text_state.font_entry = None;
                }
            },
            Op::TextRenderMode { mode } => self.text_state.mode = mode,
            Op::TextRise { rise } => self.text_state.rise = rise,
            Op::MoveTextPosition { translation } => self.text_state.translate(translation.cvt()),
            Op::SetTextMatrix { matrix } => self.text_state.set_matrix(matrix.cvt()),
            Op::TextNewline => self.text_state.next_line(),
            Op::TextDraw { ref text } => {
                let fill_mode = self.blend_mode_fill();
                let stroke_mode = self.blend_mode_stroke();

                if self._debug {
                    dbg!(text);
                }

                self.text(|backend, text_state, graphics_state, span| {
                    text_state.draw_text(backend, graphics_state, &text.data, span, fill_mode, stroke_mode);
                }, op_nr);
            },
            Op::TextDrawAdjusted { ref array } => {
                let fill_mode = self.blend_mode_fill();
                let stroke_mode = self.blend_mode_stroke();

                //let mut _debug = self._debug;

                self.text(|backend, text_state, graphics_state, span| {
                    for arg in array {
                        match *arg {
                            TextDrawAdjusted::Text(ref data) => {
                                text_state.draw_text(backend, graphics_state, data.as_bytes(), span, fill_mode, stroke_mode);
                            },
                            TextDrawAdjusted::Spacing(offset) => {
                                // because why not PDF…
                                let advance = text_state.advance(-0.001 * offset);
                                span.width += advance;
                                span.adjust += offset;
                                if span.text.is_empty() {
                                    // Nothing drawn yet: the glyphs start this
                                    // far into the text space of the span.
                                    span.leading += advance;
                                }
                            }
                        }
                    }
                    /*
                    if span.text.contains("both for") {
                        dbg!(array);
                        _debug = true;
                    }
                    if span.text.contains("those that") {
                        dbg!(array);
                        _debug = false
                    }
                    */
                }, op_nr);

                //self._debug = _debug
            },
            Op::XObject { ref name } => {
                let Some(&xobject_ref) = self.resources.xobjects.get(name) else {
                    // A content stream naming an object the page's `/Resources`
                    // does not list: one bad `Do` is not a reason to give up on
                    // the other nine hundred ops on the page.
                    warn!("xobject {name}: not in Resources");
                    return Ok(());
                };
                let xobject = match self.resolve.get(xobject_ref) {
                    Ok(x) => x,
                    Err(e) => {
                        // Same for an object that is there but is not one: a
                        // `Null` in the cross-reference table is common in
                        // files written by tools that dropped unused objects.
                        warn!("xobject {name}: {e:?}");
                        return Ok(());
                    }
                };
                let mode = self.blend_mode_fill();
                match *xobject {
                    XObject::Image(ref im) => {
                        if im.image_mask {
                            // A stencil paints in the current fill colour: the
                            // fill is not a colour the image carries, it is
                            // the one the page set before `Do`.
                            let fill = FillMode {
                                color: self.graphics_state.fill_color.clone(),
                                alpha: self.graphics_state.fill_color_alpha,
                                mode,
                            };
                            self.backend.draw_stencil(xobject_ref, im, self.resources, self.graphics_state.transform, mode, &fill, self.graphics_state.clip_path_id, self.resolve);
                        } else {
                            self.backend.draw_image(xobject_ref, im, self.resources, self.graphics_state.transform, mode, self.graphics_state.clip_path_id, self.resolve);
                        }
                    }
                    XObject::Form(ref content) => {
                        self.draw_form(content)?;
                    }
                    XObject::Postscript(ref ps) => {
                        let data = ps.data(self.resolve)?;
                        self.backend.bug_postscript(&data);
                        warn!("Got PostScript?!");
                    }
                }
            },
            Op::InlineImage { ref image } => {
                let mode = self.blend_mode_fill();
                self.backend.draw_inline_image(image, &self.resources, self.graphics_state.transform, mode, self.graphics_state.clip_path_id, self.resolve);
            }
        }

        Ok(())
    }

    fn blend_mode_fill(&self) -> BlendMode {
        if self.graphics_state.overprint_fill {
            BlendMode::Darken
        } else {
            BlendMode::Overlay
        }
    }
    fn blend_mode_stroke(&self) -> BlendMode {
        if self.graphics_state.overprint_stroke {
            BlendMode::Darken
        } else {
            BlendMode::Overlay
        }
    }

    fn text(&mut self, inner: impl FnOnce(&mut B, &mut TextState<B::Encoder>, &mut GraphicsState<B>, &mut Span), op_nr: usize) {
        let mut span = Span::default();
        let tm = self.text_state.text_matrix;

        inner(&mut self.backend, &mut self.text_state, &mut self.graphics_state, &mut span);

        let transform = self.graphics_state.transform * tm * Transform2F::from_scale(Vector2F::new(1.0, -1.0));
        // Text space has one unit per em per font size: glyph advances are
        // scaled by `font_size` (`textstate::draw_text`), and `transform`
        // holds the text matrix, not the font size — a page that writes
        // `1 Tf` and puts the size in the matrix is as common as the other
        // way round. So an em is `font_size` units high here, not one.
        let em = if self.text_state.font_size > 0.0 { self.text_state.font_size } else { 1.0 };
        // The glyphs may start after the text matrix origin (a TJ adjustment
        // before the first string), so the span starts where the ink does.
        let p1 = Vector2F::new(span.leading, -em);
        let p2 = Vector2F::new(span.width.max(span.leading), 0.0);
        let clip = self.graphics_state.clip_path_id;

        debug!("text {}", span.text);
        self.backend.add_text(TextSpan {
            rect: RectF::from_points(p1, p2),
            width: span.width - span.leading,
            text: span.text,
            chars: span.chars,
            font: self.text_state.font_entry.clone(),
            font_size: self.text_state.font_size,
            color: self.graphics_state.fill_color.clone(),
            alpha: self.graphics_state.fill_color_alpha,
            mode: self.text_state.mode,
            transform,
            op_nr,
            bytes: span.bytes
        }, clip);
    }

    /// The page rectangle in this backend's coordinates, set by the caller;
    /// see [`RenderState::draw_shading`].
    pub fn set_device_rect(&mut self, r: RectF) {
        self.device_rect = r;
    }

    /// `sh`: fill the region the current clip selects with the shading that
    /// `/Shading` names (§8.7.4.2).
    fn draw_shading(&mut self, name: &Name) -> Result<()> {
        let Some(raw) = self.resources.shading.get(name) else {
            // Like a missing XObject: one bad operator does not lose the page.
            warn!("shading {:?}: not in Resources", name);
            return Ok(());
        };
        let dict = match raw.clone().resolve(self.resolve)? {
            Primitive::Dictionary(dict) => dict,
            Primitive::Stream(stream) => stream.info,
            p => {
                warn!("shading {:?}: expected a dictionary, found {}", name, p.get_debug_name());
                return Ok(());
            }
        };
        let Some(gradient) = self.shading_gradient(&dict)? else {
            return Ok(());
        };
        self.paint_shading(gradient);
        Ok(())
    }

    /// `scn` with a pattern name gives [`Fill::Pattern`]; a *shading* pattern
    /// (`/PatternType 2`, §8.7.3.5) is not a cell to tile but a gradient to
    /// paint, and the backends paint that directly.
    fn paint_of_pattern(&self, fill: Fill) -> Result<Fill> {
        let Fill::Pattern(pattern_ref) = fill else {
            return Ok(fill);
        };
        let Ok(pattern) = self.resolve.get(pattern_ref) else {
            return Ok(Fill::Pattern(pattern_ref));
        };
        let dict = pattern.dict();
        if !dict.is_shading() {
            return Ok(Fill::Pattern(pattern_ref));
        }
        let Some(shading) = dict.shading.as_ref() else {
            return Ok(Fill::Pattern(pattern_ref));
        };
        let shading = match shading.clone().resolve(self.resolve)? {
            Primitive::Dictionary(dict) => dict,
            Primitive::Stream(stream) => stream.info,
            _ => return Ok(Fill::Pattern(pattern_ref)),
        };
        let Some(mut gradient) = self.shading_gradient(&shading)? else {
            return Ok(Fill::Pattern(pattern_ref));
        };
        // The pattern matrix replaces the one the shading dictionary has: it
        // maps pattern space, and a shading pattern always has one (default:
        // the identity).
        let m = match dict.matrix {
            Some(Matrix { a, b, c, d, e, f }) => Matrix { a, b, c, d, e, f }.cvt(),
            None => Transform2F::default(),
        };
        gradient.matrix = m;
        Ok(Fill::Gradient(Arc::new(gradient)))
    }

    /// A shading dictionary as a gradient, or `None` for the kinds this does
    /// not paint (meshes).
    fn shading_gradient(&self, dict: &Dictionary) -> Result<Option<Gradient>> {
        let Some(tp) = dict.get("ShadingType") else {
            warn!("shading without /ShadingType");
            return Ok(None);
        };
        let shading_type = tp.as_number()? as i32;
        let coords = match dict.get("Coords") {
            Some(c) => number_array(c)?,
            None => {
                warn!("shading type {shading_type} without /Coords");
                return Ok(None);
            }
        };
        let kind = match (shading_type, coords.len()) {
            (2, 4) => GradientKind::Linear {
                start: Vector2F::new(coords[0], coords[1]),
                end: Vector2F::new(coords[2], coords[3]),
            },
            (3, 6) => GradientKind::Radial {
                start: Vector2F::new(coords[0], coords[1]),
                start_radius: coords[2],
                end: Vector2F::new(coords[3], coords[4]),
                end_radius: coords[5],
            },
            (n, _) => {
                // 1, 4, 5, 6 and 7 are meshes: streams of triangles, not a
                // paint that varies with one parameter. Nothing here paints
                // them yet.
                warn!("shading type {n} not painted (only 2 and 3 are)");
                return Ok(None);
            }
        };
        // §8.7.4.3: two booleans, "extend beyond the start / the end". True
        // paints that side with the colour of the nearest endpoint, false
        // leaves it unpainted. Not `repeat`: a shading has no tiling.
        let extend = match dict.get("Extend") {
            Some(Primitive::Array(a)) if a.len() == 2 => {
                let e = |p: &Primitive| {
                    if p.as_bool().unwrap_or(false) { Extend::Pad } else { Extend::None }
                };
                [e(&a[0]), e(&a[1])]
            }
            // The default is [false false]: a shading with no /Extend paints
            // its domain and nothing else.
            _ => [Extend::None; 2],
        };
        // The coordinates are in the user space in force at the `sh`, so the
        // matrix is the identity there; the CTM is applied when the shape is
        // drawn, not here. A `/Matrix` entry is not part of a type 2 or 3
        // shading dictionary (Table 78 — only function-based shadings have
        // one), and poppler and gs both ignore it when a generator writes one
        // anyway; a file that rotates a shading does it with `cm`, which is in
        // the CTM already.
        let matrix = Transform2F::default();

        let Some(cs) = dict.get("ColorSpace") else {
            warn!("shading without /ColorSpace");
            return Ok(None);
        };
        let cs = ColorSpace::from_primitive(cs.clone(), self.resolve)?;
        let cs = device_color_space(&cs);
        let Some(func) = dict.get("Function") else {
            warn!("shading without /Function");
            return Ok(None);
        };
        let func = Function::from_primitive(func.clone(), self.resolve)?;

        // A shading's `/Function` maps the parameter to colour; sampling it
        // uniformly and keeping the points where the colour changes turns it
        // into stops, whatever the function type (a sampled table, an
        // exponent, a stitching chain) — the backends only know stops.
        const SAMPLES: usize = 256;
        let mut out = vec![0.0; func.output_dim()];
        let mut stops: Vec<Stop> = Vec::new();
        for i in 0..SAMPLES {
            let t = i as f32 / (SAMPLES - 1) as f32;
            func.apply(&[t], &mut out)?;
            let color = self.components_to_rgb(cs, &out)?;
            if stops.last().map_or(true, |s| s.color != color) {
                stops.push(Stop { offset: t, color });
            }
        }
        match stops.len() {
            0 => return Ok(None),
            1 => {
                let c = stops[0].color;
                stops.push(Stop { offset: 1.0, color: c });
            }
            _ => {
                // The last run covers everything up to 1.0 by construction;
                // giving it that offset is what `/Extend` pad means.
                if let Some(last) = stops.last_mut() {
                    last.offset = 1.0;
                }
            }
        }
        Ok(Some(Gradient { kind, stops, extend, matrix }))
    }

    /// A shading colour, given in components of its own colour space, in the
    /// renderer's rgb. Goes through the same conversion as `sc`/`scn`, so
    /// `/Separation`, `/Indexed`, `/ICCBased` and `/DeviceN` shadings come out
    /// as the rest of the page does.
    fn components_to_rgb(&self, cs: &ColorSpace, comps: &[f32]) -> Result<(f32, f32, f32)> {
        let args = comps.iter().map(|&c| Primitive::Number(c)).collect();
        let mut cs = cs;
        match convert_color2(&mut cs, &Color::Other(args), self.resources, BlendMode::Overlay)? {
            Fill::Solid(r, g, b) => Ok((r, g, b)),
            _ => Ok((0.0, 0.0, 0.0)),
        }
    }

    /// Draw a shading over the region §8.7.4.2 says `sh` paints: what is
    /// currently clipped. The page is the largest region it can cover; the
    /// backend clips it to the real one.
    fn paint_shading(&mut self, gradient: Gradient) {
        // A gradient paint pads both ends, but `/Extend false` means the
        // region beyond that end is painted with nothing at all. No gradient
        // paint can express that, so the shape itself stops at the end.
        let (outline, fill_rule) = self.shading_shape(&gradient);
        let fill = FillMode {
            color: Fill::Gradient(Arc::new(gradient)),
            alpha: self.graphics_state.fill_color_alpha,
            mode: self.blend_mode_fill(),
        };
        // The shape is in user space — the page, or the part of it the
        // parameter range covers — and is drawn with the CTM like any other
        // path, so the gradient's own matrix stays the identity.
        self.backend.draw(
            &outline,
            &DrawMode::Fill { fill },
            fill_rule,
            self.graphics_state.transform,
            self.graphics_state.clip_path_id,
        );
    }

    /// The area a shading paints, in device space.
    ///
    /// With `/Extend [true true]` that is the page. With an end switched off
    /// it is only the parameter domain: the strip between the two lines
    /// perpendicular to an axial gradient's axis, or the annulus (the disk
    /// when `r0` is 0) of a radial one. The shape is built in shading space
    /// and mapped through the gradient's matrix, so a rotated or skewed CTM
    /// gives a rotated strip or an ellipse, not a bounding box.
    fn shading_shape(&self, g: &Gradient) -> (Outline, FillRule) {
        // The page, in the user space the shading's coordinates are in.
        let ctm = self.graphics_state.transform;
        let page = ctm.inverse() * self.device_rect;
        if g.extend == [Extend::Pad, Extend::Pad] {
            return (Outline::from_rect(page), FillRule::Winding);
        }
        // Sizes in shading space: how far "beyond the end" has to reach. The
        // page is the only thing that limits the paint, so its diagonal is
        // the measure, taken back into shading space.
        let inverse = g.matrix.inverse();
        if !inverse.m11().is_finite() {
            // A singular matrix has no "beyond the end": paint the page and
            // let the gradient be degenerate there.
            return (Outline::from_rect(page), FillRule::Winding);
        }
        let corners = [
            page.origin(),
            page.upper_right(),
            page.lower_right(),
            page.lower_left(),
        ];
        let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
        let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
        for c in corners {
            let p = inverse * c;
            min_x = min_x.min(p.x());
            min_y = min_y.min(p.y());
            max_x = max_x.max(p.x());
            max_y = max_y.max(p.y());
        }
        let reach = 4.0 * ((max_x - min_x) + (max_y - min_y)).max(1.0);

        let mut outline = Outline::new();
        match g.kind {
            GradientKind::Linear { start, end } => {
                let d = end - start;
                let len = d.length();
                if len <= 0.0 {
                    return (Outline::from_rect(page), FillRule::Winding);
                }
                let u = d / len;
                let n = Vector2F::new(-u.y(), u.x()) * reach;
                let a = if g.extend[0] == Extend::Pad { start - u * reach } else { start };
                let b = if g.extend[1] == Extend::Pad { end + u * reach } else { end };
                let quad = [a - n, b - n, b + n, a + n];
                push_polygon(&mut outline, &quad, &g.matrix);
                (outline, FillRule::Winding)
            }
            GradientKind::Radial { start, start_radius, end, end_radius } => {
                // Even-odd: the outer circle minus the inner one. A circle is
                // a polygon here only because `Outline` has no arc; 96 sides
                // is under a third of a device pixel of chord error on a page
                // of shading size.
                let mut circle = |c: Vector2F, r: f32, out: &mut Outline| {
                    if r <= 0.0 {
                        return;
                    }
                    const SIDES: usize = 96;
                    let mut pts = Vec::with_capacity(SIDES);
                    for i in 0..SIDES {
                        let t = i as f32 / SIDES as f32 * std::f32::consts::TAU;
                        pts.push(Vector2F::new(c.x() + r * t.cos(), c.y() + r * t.sin()));
                    }
                    push_polygon(out, &pts, &g.matrix);
                };
                circle(end, end_radius, &mut outline);
                circle(start, start_radius, &mut outline);
                if outline.contours().is_empty() {
                    return (Outline::from_rect(page), FillRule::Winding);
                }
                (outline, FillRule::EvenOdd)
            }
        }
    }

    fn color_space(&self, name: &str) -> Result<&'a ColorSpace> {
        match name {
            "DeviceGray" => return Ok(&ColorSpace::DeviceGray),
            "DeviceRGB" => return Ok(&ColorSpace::DeviceRGB),
            "DeviceCMYK" => return Ok(&ColorSpace::DeviceCMYK),
            "Pattern" => return Ok(&ColorSpace::Pattern),
            _ => {}
        }
        match self.resources.color_spaces.get(name) {
            Some(cs) => Ok(cs),
            None => Err(PdfError::Other { msg: format!("color space {:?} not present", name) })
        }
    }
    fn flush(&mut self) {
        if !self.current_contour.is_empty() {
            self.current_outline.push_contour(self.current_contour.clone());
            self.current_contour.clear();
        }
    }
    fn draw_form(&mut self, form: &FormXObject) -> Result<()> {
        let graphics_state = GraphicsState {
            stroke_alpha: self.graphics_state.stroke_color_alpha,
            fill_alpha: self.graphics_state.fill_color_alpha,
            clip_path_id: self.graphics_state.clip_path_id,
            .. self.graphics_state.clone()
        };
        let resources = match form.dict().resources {
            Some(ref r) => &*r,
            None => self.resources
        };

        let mut inner = RenderState {
            graphics_state: graphics_state,
            text_state: self.text_state.clone(),
            resources,
            stack: vec![],
            current_outline: Outline::new(),
            current_contour: Contour::new(),
            backend: self.backend,
            resolve: self.resolve,
            // A form paints into the same device area as the page it is part
            // of; a form may itself contain `sh`, which paints over the clip
            // in force, so it needs the rectangle too.
            device_rect: self.device_rect,
            _debug: self._debug,
        };

        for (i, op) in form.ops(self.resolve).enumerate() {
            let op = t!(op);
            debug!(" form op {}: {:?}", i, op);
            inner.draw_op(&op, i)?;
        }

        Ok(())
    }
    #[allow(dead_code)]
    fn get_properties<'b>(&'b self, p: &'b Primitive) -> Result<&'b Dictionary> {
        match p {
            Primitive::Dictionary(dict) => Ok(dict),
            Primitive::Name(name) => self.resources.properties.get(name.as_str())
                .map(|rc| &**rc)
                .ok_or_else(|| {
                    PdfError::MissingEntry { typ: "Properties", field: name.into() }
                }),
            p => Err(PdfError::UnexpectedPrimitive {
                expected: "Dictionary or Name",
                found: p.get_debug_name()
            })
        }
    }
}

/// One closed polygon contour, in device space.
fn push_polygon(outline: &mut Outline, points: &[Vector2F], m: &Transform2F) {
    if points.len() < 3 {
        return;
    }
    let mut contour = Contour::new();
    for &p in points {
        contour.push_endpoint(*m * p);
    }
    let first = *m * points[0];
    contour.push_endpoint(first);
    outline.push_contour(contour);
}

/// The numbers of a `/Coords` or `/Matrix` array.
fn number_array(p: &Primitive) -> Result<Vec<f32>> {
    match p {
        Primitive::Array(a) => a.iter().map(|p| p.as_number()).collect(),
        p => Err(PdfError::UnexpectedPrimitive { expected: "Array", found: p.get_debug_name() }),
    }
}

/// `/DeviceRGB` in a shading's `/ColorSpace` is a name like any other, and
/// the conversion below looks names up in `/Resources /ColorSpace`, where the
/// device spaces are never listed.
fn device_color_space(cs: &ColorSpace) -> &ColorSpace {
    match cs {
        ColorSpace::Named(name) => match name.as_str() {
            "DeviceGray" | "CalGray" => &ColorSpace::DeviceGray,
            "DeviceRGB" | "CalRGB" => &ColorSpace::DeviceRGB,
            "DeviceCMYK" | "CalCMYK" => &ColorSpace::DeviceCMYK,
            _ => cs,
        },
        _ => cs,
    }
}

fn convert_color<'a>(cs: &mut &'a ColorSpace, color: &Color, resources: &Resources, resolve: &impl Resolve, mode: BlendMode) -> Result<Fill> {
    match convert_color2(cs, color, resources, mode) {
        Ok(color) => Ok(color),
        Err(e) if resolve.options().allow_error_in_option => {
            warn!("failed to convert color: {:?}", e);
            Ok(Fill::Solid(0.0, 0.0, 0.0))
        }
        Err(e) => Err(e)
    }
}
#[allow(unused_variables)]
fn convert_color2<'a>(cs: &mut &'a ColorSpace, color: &Color, resources: &Resources, mode: BlendMode) -> Result<Fill> {
    match *color {
        Color::Gray(g) => {
            *cs = &ColorSpace::DeviceGray;
            Ok(gray2rgb(g))
        }
        Color::Rgb(rgb) => {
            *cs = &ColorSpace::DeviceRGB;
            let (r, g, b) = rgb.cvt();
            Ok(Fill::Solid(r, g, b))
        }
        Color::Cmyk(cmyk) => {
            *cs = &ColorSpace::DeviceCMYK;
            Ok(cmyk2rgb(cmyk.cvt(), mode))
        }
        Color::Other(ref args) => {
            let cs = match **cs {
                ColorSpace::Icc(ref icc) => {
                    match icc.info.alternate {
                        Some(ref alt) => alt,
                        // No /Alternate: the profile's own component count says
                        // how many operands there are, and 8.6.5.5 makes such a
                        // space behave as the device space with the same count.
                        // Reading it as an error left whole pages black — the
                        // one-component profiles are common (an /ICCBased grey
                        // for a page background).
                        None => {
                            match icc.info.components {
                                1 => &ColorSpace::DeviceGray,
                                3 => &ColorSpace::DeviceRGB,
                                4 => &ColorSpace::DeviceCMYK,
                                n => return Err(PdfError::Other { msg: format!("ICC profile with {} components has no alternate color space", n) })
                            }
                        }
                    }
                }
                ColorSpace::Named(ref name) => {
                    resources.color_spaces.get(name).ok_or_else(||
                        PdfError::Other { msg: format!("named color space {} not found", name) }
                    )?
                }
                _ => &**cs
            };

            match *cs {
                ColorSpace::Icc(_) => return Err(PdfError::Other { msg: format!("nested ICC color space") }),
                ColorSpace::Lab(ref lab) => {
                    if args.len() != 3 {
                        return Err(PdfError::Other { msg: format!("expected 3 color arguments, got {:?}", args) });
                    }
                    let l = args[0].as_number()?;
                    let a = args[1].as_number()?;
                    let b = args[2].as_number()?;
                    Ok(lab2rgb(l, a, b, lab))
                }
                ColorSpace::DeviceGray | ColorSpace::CalGray(_) => {
                    if args.len() != 1 {
                        return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                    }
                    let g = args[0].as_number()?;
                    Ok(gray2rgb(g))
                }
                ColorSpace::DeviceRGB | ColorSpace::CalRGB(_) => {
                    if args.len() != 3 {
                        return Err(PdfError::Other { msg: format!("expected 3 color arguments, got {:?}", args) });
                    }
                    let r = args[0].as_number()?;
                    let g = args[1].as_number()?;
                    let b = args[2].as_number()?;
                    Ok(Fill::Solid(r, g, b))
                }
                ColorSpace::DeviceCMYK | ColorSpace::CalCMYK(_) => {
                    if args.len() != 4 {
                        return Err(PdfError::Other { msg: format!("expected 4 color arguments, got {:?}", args) });
                    }
                    let c = args[0].as_number()?;
                    let m = args[1].as_number()?;
                    let y = args[2].as_number()?;
                    let k = args[3].as_number()?;
                    Ok(cmyk2rgb((c, m, y, k), mode))
                }
                ColorSpace::DeviceN { ref names, ref alt, ref tint, ref attr } => {
                    assert_eq!(args.len(), tint.input_dim());
                    let mut input = vec![0.; args.len()];
                    for (i, a) in input.iter_mut().zip(args.iter()) {
                        *i = a.as_number()?;
                    }
                    let mut out = vec![0.0; tint.output_dim()];
                    tint.apply(&input, &mut out)?;

                    let alt = match **alt {
                        ColorSpace::Icc(ref icc) => icc.info.alternate.as_ref().map(|b| &**b),
                        ref a => Some(a),
                    };
                    match alt {
                        Some(ColorSpace::DeviceGray) => Ok(Fill::Solid(out[0], out[0], out[0])),
                        Some(ColorSpace::DeviceRGB) => {
                            Ok(Fill::Solid(out[0], out[1], out[2]))
                        }
                        Some(ColorSpace::DeviceCMYK) => {
                            Ok(cmyk2rgb((out[0], out[1], out[2], out[3]), mode))
                        }
                        _ => {
                            warn!("DeviceN alternate {:?} not supported, using its first components", alt);
                            Ok(gray2rgb(out[0]))
                        }
                    }
                }
                ColorSpace::Separation(ref name, ref alt, ref f) => {
                    debug!("Separation(name={}, alt={:?}, f={:?}", name, alt, f);
                    if args.len() != 1 {
                        return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                    }
                    let x = args[0].as_number()?;
                    let cs = match **alt {
                        ColorSpace::Icc(ref info) => &**info.alternate.as_ref().ok_or(
                            PdfError::Other { msg: format!("no alternate color space in ICC profile {:?}", info) }
                        )?,
                        _ => alt,
                    };
                    match cs {
                        &ColorSpace::DeviceCMYK => {
                            let mut cmyk = [0.0; 4];
                            f.apply(&[x], &mut cmyk)?;
                            let [c, m, y, k] = cmyk;
                            //debug!("c={c}, m={m}, y={y}, k={k}");
                            Ok(cmyk2rgb((c, m, y, k), mode))
                        },
                        &ColorSpace::DeviceRGB => {
                            let mut rgb = [0.0, 0.0, 0.0];
                            f.apply(&[x], &mut rgb)?;
                            let [r, g, b] = rgb;
                            //debug!("r={r}, g={g}, b={b}");
                            Ok(Fill::Solid(r, g, b))
                        },
                        &ColorSpace::DeviceGray => {
                            let mut gray = [0.0];
                            f.apply(&[x], &mut gray)?;
                            let [gray] = gray;
                            //debug!("gray={gray}");
                            Ok(Fill::Solid(gray, gray, gray))
                        }
                        c => {
                            warn!("Separation alternate {:?} not supported, using the tint as grey", c);
                            let mut gray = [0.0];
                            let _ = f.apply(&[x], &mut gray);
                            Ok(gray2rgb(gray[0]))
                        }
                    }
                }
                ColorSpace::Indexed(ref cs, hival, ref lut) => {
                    if args.len() != 1 {
                        return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                    }
                    let i = args[0].as_integer()?;
                    match **cs {
                        ColorSpace::DeviceRGB => {
                            let c = &lut[3 * i as usize ..];
                            let cvt = |b: u8| b as f32;
                            Ok(Fill::Solid(cvt(c[0]), cvt(c[1]), cvt(c[2])))
                        }
                        ColorSpace::DeviceCMYK => {
                            let c = &lut[4 * i as usize ..];
                            let cvt = |b: u8| b as f32;
                            Ok(cmyk2rgb((cvt(c[0]), cvt(c[1]), cvt(c[2]), cvt(c[3])), mode))
                        }
                        ref base => {
                            warn!("Indexed colorspace with base {:?} not supported, using black", base);
                            Ok(Fill::Solid(0., 0., 0.))
                        }
                    }
                }
                ColorSpace::Pattern => {
                    // The pattern name is the last operand: for an uncolored
                    // pattern the color components come first (PDF 32000-1,
                    // 8.7.3.4 `c1 … cn name scn`), and files in the wild do
                    // use `/Pattern cs` for those.
                    let name = match args.last().map(|p| p.as_name()) {
                        Some(Ok(name)) => name,
                        _ => {
                            return Err(PdfError::Other {
                                msg: format!("pattern color space needs a pattern name, got {:?}", args),
                            })
                        }
                    };
                    match resources.pattern.get(name) {
                        Some(&pat) => Ok(Fill::Pattern(pat)),
                        None => {
                            warn!("pattern {} not found, using black", name);
                            Ok(Fill::Solid(0., 0., 0.))
                        }
                    }
                }
                ColorSpace::Other(ref p) => {
                    warn!("color space {:?} not supported, using black", p);
                    Ok(Fill::Solid(0., 0., 0.))
                }
                ColorSpace::Named(ref p) => {
                    warn!("nested named color space {:?} not supported, using black", p);
                    Ok(Fill::Solid(0., 0., 0.))
                }
            }
        }
    }
}

fn gray2rgb(g: f32) -> Fill {
    Fill::Solid(g, g, g)
}

/// CIE 1976 L*a*b* to sRGB, using the space's own white point and the D50
/// adaptation sRGB's matrix expects (8.6.5.4 gives the parameters, sRGB the
/// rest). A fill colour needs to be close, not exact: three levels of error on
/// a tint are invisible, a page of black because the space was "not supported"
/// is not.
fn lab2rgb(l: f32, a: f32, b: f32, params: &LabParams) -> Fill {
    let wp = |i: usize, default: f32| params.white_point.get(i).copied().unwrap_or(default);
    let (xw, yw, zw) = (wp(0, 0.9642), wp(1, 1.0), wp(2, 0.8249));
    // 8.6.5.4: a and b span [amin, amax] and [bmin, bmax], which map onto the
    // standard -128..127 of the a*b* plane.
    let (amin, amax, bmin, bmax) = match params.range {
        Some(ref r) if r.len() >= 4 => (r[0], r[1], r[2], r[3]),
        _ => (-100.0, 100.0, -100.0, 100.0),
    };
    let a = if amax > amin { -128.0 + (a - amin) * 255.0 / (amax - amin) } else { a };
    let b = if bmax > bmin { -128.0 + (b - bmin) * 255.0 / (bmax - bmin) } else { b };
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let finv = |f: f32| {
        let f3 = f * f * f;
        if f3 > 0.008856 { f3 } else { (116.0 * f - 16.0) / 903.3 }
    };
    let (x, y, z) = (xw * finv(fx), yw * finv(fy), zw * finv(fz));
    // Bradford, D50 -> D65, then the sRGB primaries.
    let x65 = 0.9555766 * x - 0.0230393 * y + 0.0631636 * z;
    let y65 = -0.0282895 * x + 1.0099416 * y + 0.0210077 * z;
    let z65 = 0.0122982 * x - 0.0204830 * y + 1.3299098 * z;
    let lin = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    Fill::Solid(
        lin(3.2404542 * x65 - 1.5371385 * y65 - 0.4985314 * z65),
        lin(-0.9692660 * x65 + 1.8760108 * y65 + 0.0415560 * z65),
        lin(0.0556434 * x65 - 0.2040259 * y65 + 1.0572252 * z65),
    )
}

fn cmyk2rgb((c, m, y, k): (f32, f32, f32, f32), _mode: BlendMode) -> Fill {
    // DeviceCMYK has no display conversion of its own: go through the default
    // press profile the way ghostscript and poppler do (see crate::cmyk).
    let [r, g, b] = crate::cmyk::to_rgb(c, m, y, k);
    Fill::Solid(r, g, b)
}


fn to_rect(o: &Outline) -> Option<RectF> {
    if o.contours().len() != 1 {
        return None;
    }

    let c = &o.contours()[0];
    if c.len() != 4 {
        return None;
    }

    if !c.iter(ContourIterFlags::IGNORE_CLOSE_SEGMENT).all(|segment| {
        let line = segment.baseline;
        segment.is_line() && (line.from_x() == line.to_x()) ^ (line.from_y() == line.to_y())
    }) {
        return None;
    }

    Some(c.bounds())
}
