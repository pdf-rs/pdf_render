use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::primitive::{Primitive, Dictionary};
use pdf::backend::Backend as PdfBackend;
use pdf::content::{Op, Matrix, Point, Rect, Color, Rgb, Cmyk, Winding, FormXObject};
use pdf::error::{PdfError, Result};
use pdf::content::TextDrawAdjusted;
use crate::backend::Backend;

use pathfinder_geometry::{
    vector::{Vector2F},
    rect::RectF, transform2d::{Transform2F, Matrix2x2F},
};
use pathfinder_content::{
    fill::FillRule,
    stroke::{LineCap, LineJoin, StrokeStyle},
    outline::{Outline, Contour},
    pattern::{Pattern, Image},
};
use pathfinder_color::{ColorU, ColorF};
use super::{
    graphicsstate::{GraphicsState},
    textstate::{TextState},
    BBox,
    DrawMode,
};

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
    graphics_state: GraphicsState<'a>,
    text_state: TextState,
    stack: Vec<(GraphicsState<'a>, TextState)>,
    current_outline: Outline,
    current_contour: Contour,
    resolve: &'a R,
    resources: &'a Resources,
    backend: &'a mut B,
}

impl<'a, R: Resolve, B: Backend> RenderState<'a, R, B> {
    pub fn new(backend: &'a mut B, resolve: &'a R, resources: &'a Resources, root_transformation: Transform2F) -> Self {
        let graphics_state = GraphicsState {
            transform: root_transformation,
            fill_color: ColorF::black(),
            fill_paint: None,
            fill_alpha: 1.0,
            stroke_color: ColorF::black(),
            stroke_paint: None,
            stroke_alpha: 1.0,
            clip_path: None,
            clip_path_id: None,
            fill_color_space: &ColorSpace::DeviceRGB,
            stroke_color_space: &ColorSpace::DeviceRGB,
            stroke_style: StrokeStyle {
                line_cap: LineCap::Butt,
                line_join: LineJoin::Miter(1.0),
                line_width: 1.0,
            }
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
        }
    }
    fn draw(&mut self, mode: DrawMode, fill_rule: FillRule) {
        self.flush();
        self.backend.draw(&self.current_outline, mode, fill_rule, self.graphics_state.transform);
        self.current_outline.clear();
    }
    pub fn draw_op(&mut self, op: &Op) -> Result<()> {
        match *op {
            Op::BeginMarkedContent { .. } => {}
            Op::EndMarkedContent { .. } => {}
            Op::MarkedContentPoint { .. } => {}
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
                self.draw(DrawMode::Stroke(
                    self.graphics_state.stroke_color,
                    self.graphics_state.stroke_style
                ), FillRule::Winding);
            },
            Op::FillAndStroke { winding } => {
                self.draw(DrawMode::FillStroke(
                    self.graphics_state.fill_color,
                    self.graphics_state.stroke_color,
                    self.graphics_state.stroke_style
                ), winding.cvt());
            }
            Op::Fill { winding } => {
                self.draw(DrawMode::Fill(self.graphics_state.fill_color), winding.cvt());
            }
            Op::Shade { ref name } => {},
            Op::Clip { winding } => {
                self.flush();
                let path = self.current_outline.clone().transformed(&self.graphics_state.transform);
                //self.debug_outline(path.clone(), ColorU::new(0, 0, 255, 50));

                self.graphics_state.merge_clip_path(path, winding.cvt());

                //let o = self.graphics_state.clip_path.as_ref().unwrap().outline().clone();
                //self.debug_outline(o, ColorU::new(255, 0, 0, 50));
            },

            Op::Save => {
                self.stack.push((self.graphics_state.clone(), self.text_state.clone()));
            },
            Op::Restore => {
                let (g, t) = self.stack.pop().expect("graphcs stack is empty");
                self.graphics_state = g;
                self.text_state = t;
            },

            Op::Transform { matrix } => {
                self.graphics_state.transform = self.graphics_state.transform * matrix.cvt();
            }
            Op::LineWidth { width } => self.graphics_state.stroke_style.line_width = width,
            Op::Dash { ref pattern, phase } => {},
            Op::LineJoin { join } => {},
            Op::LineCap { cap } => {},
            Op::MiterLimit { limit } => {},
            Op::Flatness { tolerance } => {},
            Op::GraphicsState { ref name } => {
                let gs = try_opt!(self.resources.graphics_states.get(name));
                if let Some(lw) = gs.line_width {
                    self.graphics_state.stroke_style.line_width = lw;
                }
                self.graphics_state.set_fill_alpha(gs.fill_alpha.unwrap_or(1.0));
                self.graphics_state.set_stroke_alpha(gs.stroke_alpha.unwrap_or(1.0));
                
                if let Some((font_ref, size)) = gs.font {
                    if let Some(e) = self.backend.get_font(font_ref, self.resolve)? {
                        debug!("new font: {} at size {}", e.name, size);
                        self.text_state.font_entry = Some(e);
                        self.text_state.font_size = size;
                    } else {
                        self.text_state.font_entry = None;
                    }
                }
            },
            Op::StrokeColor { ref color } => {
                let color = t!(convert_color(&mut self.graphics_state.stroke_color_space, color, &self.resources));
                self.graphics_state.set_stroke_color(color);
            },
            Op::FillColor { ref color } => {
                let color = t!(convert_color(&mut self.graphics_state.fill_color_space, color, &self.resources));
                self.graphics_state.set_fill_color(color);
            },
            Op::FillColorSpace { ref name } => {
                self.graphics_state.fill_color_space = self.color_space(name)?;
                self.graphics_state.set_fill_color((0., 0., 0.));
            },
            Op::StrokeColorSpace { ref name } => {
                self.graphics_state.stroke_color_space = self.color_space(name)?;
                self.graphics_state.set_stroke_color((0., 0., 0.));
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
                    Some(&font_ref) => {
                        self.backend.get_font(font_ref, self.resolve)?
                    },
                    None => None
                };
                if let Some(e) = font {
                    debug!("new font: {} (is_cid={:?})", e.name, e.is_cid);
                    self.text_state.font_entry = Some(e);
                    self.text_state.font_size = size;
                } else {
                    warn!("no font {}", name);
                    self.text_state.font_entry = None;
                }
            },
            Op::TextRenderMode { mode } => self.text_state.mode = mode,
            Op::TextRise { rise } => self.text_state.rise = rise,
            Op::MoveTextPosition { translation } => self.text_state.translate(translation.cvt()),
            Op::SetTextMatrix { matrix } => self.text_state.set_matrix(matrix.cvt()),
            Op::TextNewline => self.text_state.next_line(),
            Op::TextDraw { ref text } => {
                self.text_state.draw_text(self.backend, &self.graphics_state, &text.data);
            },
            Op::TextDrawAdjusted { ref array } => {
                let mut bb = BBox::empty();
                for arg in array {
                    match *arg {
                        TextDrawAdjusted::Text(ref data) => {
                            self.text_state.draw_text(self.backend, &self.graphics_state, data.as_bytes());
                        },
                        TextDrawAdjusted::Spacing(offset) => {
                            self.text_state.advance(-0.001 * offset); // because why not PDF…
                        }
                    }
                }
            },
            Op::XObject { ref name } => {
                let &xobject_ref = self.resources.xobjects.get(name).ok_or(PdfError::NotFound { word: name.into()})?;
                let xobject = self.resolve.get(xobject_ref)?;
                match *xobject {
                    XObject::Image(ref im) => {
                        self.backend.draw_image(xobject_ref, im, self.graphics_state.transform, self.resolve);
                    }
                    XObject::Form(ref content) => {
                        self.draw_form(content)?;
                    }
                    XObject::Postscript(ref ps) => {
                        warn!("Got PostScript?!");
                    }
                }
            },
            Op::InlineImage { .. } => {}
        }

        Ok(())
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
            stroke_alpha: self.graphics_state.stroke_color.a(),
            fill_alpha: self.graphics_state.fill_color.a(),
            clip_path: self.graphics_state.clip_path.clone(),
            .. self.graphics_state
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
        };
        
        let ops = form.operations(self.resolve)?;
        for (i, op) in ops.iter().enumerate() {
            //debug!(" form op {}: {:?}", i, op);
            inner.draw_op(op)?;
        }

        Ok(())
    }
    fn get_properties<'b>(&'b self, p: &'b Primitive) -> Result<&'b Dictionary> {
        match p {
            Primitive::Dictionary(ref dict) => Ok(dict),
            Primitive::Name(ref name) => self.resources.properties.get(name)
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

fn convert_color<'a>(cs: &mut &'a ColorSpace, color: &Color, resources: &Resources) -> Result<(f32, f32, f32)> {
    match *color {
        Color::Gray(g) => {
            *cs = &ColorSpace::DeviceGray;
            Ok(gray2rgb(g))
        }
        Color::Rgb(rgb) => {
            *cs = &ColorSpace::DeviceRGB;
            Ok(rgb.cvt())
        }
        Color::Cmyk(cmyk) => {
            *cs = &ColorSpace::DeviceCMYK;
            Ok(cmyk2rgb(cmyk.cvt()))
        }
        Color::Other(ref args) => {
            let cs = match **cs {
                ColorSpace::Icc(ref icc) => {
                    match icc.info.alternate {
                        Some(ref alt) => alt,
                        None => return Err(PdfError::Other { msg: format!("ICC profile without alternate color space") }),
                    }
                }
                _ => &**cs
            };
            
            match *cs {
                ColorSpace::Icc(_) => return Err(PdfError::Other { msg: format!("nested ICC color space") }),
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
                    Ok((r, g, b))
                }
                ColorSpace::DeviceCMYK | ColorSpace::CalCMYK(_) => {
                    if args.len() != 4 {
                        return Err(PdfError::Other { msg: format!("expected 4 color arguments, got {:?}", args) });
                    }
                    let c = args[0].as_number()?;
                    let m = args[1].as_number()?;
                    let y = args[2].as_number()?;
                    let k = args[3].as_number()?;
                    Ok(cmyk2rgb((c, m, y, k)))
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
                        Some(ColorSpace::DeviceGray) => Ok((out[0], out[0], out[0])),
                        Some(ColorSpace::DeviceRGB) => {
                            Ok((out[0], out[1], out[2]))
                        }
                        Some(ColorSpace::DeviceCMYK) => {
                            Ok(cmyk2rgb((out[0], out[1], out[2], out[3])))
                        }
                        _ => unimplemented!("DeviceN colorspace")
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
                            Ok(cmyk2rgb((c, m, y, k)))
                        },
                        &ColorSpace::DeviceRGB => {
                            let mut rgb = [0.0, 0.0, 0.0];
                            f.apply(&[x], &mut rgb)?;
                            let [r, g, b] = rgb;
                            //debug!("r={r}, g={g}, b={b}");
                            Ok((r, g, b))
                        },
                        &ColorSpace::DeviceGray => {
                            let mut gray = [0.0];
                            f.apply(&[x], &mut gray)?;
                            let [gray] = gray;
                            //debug!("gray={gray}");
                            Ok((gray, gray, gray))
                        }
                        c => unimplemented!("Separation(alt={:?})", c)
                    }
                }
                ColorSpace::Indexed(ref cs, ref lut) => {
                    if args.len() != 1 {
                        return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                    }
                    let i = args[0].as_integer()?;
                    match **cs {
                        ColorSpace::DeviceRGB => {
                            let c = &lut[3 * i as usize ..];
                            let cvt = |b: u8| b as f32;
                            Ok((cvt(c[0]), cvt(c[1]), cvt(c[2])))
                        }
                        ColorSpace::DeviceCMYK => {
                            let c = &lut[4 * i as usize ..];
                            let cvt = |b: u8| b as f32;
                            Ok(cmyk2rgb((cvt(c[0]), cvt(c[1]), cvt(c[2]), cvt(c[3]))))
                        }
                        ref base => unimplemented!("Indexed colorspace with base {:?}", base)
                    }
                }
                ColorSpace::Pattern => {
                    let name = args[0].as_name()?;
                    //dbg!(&resources.pattern);
                    unimplemented!("Pattern {}", name)
                }
                ColorSpace::Other(ref p) => unimplemented!("Other Color space {:?}", p)
            }
        }
    }
}

fn gray2rgb(g: f32) -> (f32, f32, f32) {
    (g, g, g)
}

fn cmyk2rgb((c, m, y, k): (f32, f32, f32, f32)) -> (f32, f32, f32) {
    let clamp = |f| if f > 1.0 { 1.0 } else { f };
    (
        1.0 - clamp(c + k),
        1.0 - clamp(m + k),
        1.0 - clamp(y + k),
    )
}
