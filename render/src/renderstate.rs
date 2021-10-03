use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::primitive::{Primitive, Dictionary};
use pdf::backend::Backend;
use pdf::content::{Op, Matrix, Point, Rect, Color, Rgb, Cmyk, Winding, FormXObject};
use pdf::error::{PdfError, Result};
use pdf::content::TextDrawAdjusted;

use pathfinder_geometry::{
    vector::{Vector2F},
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_content::{
    fill::FillRule,
    stroke::{LineCap, LineJoin, StrokeStyle},
    outline::{Outline, Contour},
    pattern::{Pattern, Image},
};
use pathfinder_color::{ColorU, ColorF};
use pathfinder_renderer::{
    scene::{DrawPath, Scene},
    paint::{Paint},
};

use super::{
    graphicsstate::{GraphicsState, DrawMode},
    textstate::{TextState},
    cache::{Cache, Tracer, ItemMap, TextSpan},
    BBox,
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

pub struct RenderState<'a, B: Backend> {
    graphics_state: GraphicsState<'a>,
    text_state: TextState,
    stack: Vec<(GraphicsState<'a>, TextState)>,
    current_outline: Outline,
    current_contour: Contour,
    scene: &'a mut Scene,
    file: &'a PdfFile<B>,
    resources: &'a Resources,
    cache: &'a mut Cache,
    items: ItemMap,
}

impl<'a, B: Backend> RenderState<'a, B> {
    pub fn new(scene: &'a mut Scene, cache: &'a mut Cache, file: &'a PdfFile<B>, resources: &'a Resources, root_transformation: Transform2F) -> Self {
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

        let items = ItemMap::new();

        RenderState {
            graphics_state,
            text_state,
            stack,
            current_outline,
            current_contour,
            scene,
            resources,
            file,
            items,
            cache,
        }
    }
    pub fn draw_op(&mut self, op: &Op, tracer: &mut Tracer) -> Result<()> {
        match *op {
            Op::BeginMarkedContent { .. } => {}
            Op::EndMarkedContent { .. } => {}
            Op::MarkedContentPoint { .. } => {}
            Op::Close => {
                self.current_contour.close();
                tracer.stash_multi();
            }
            Op::MoveTo { p } => {
                self.flush();
                self.current_contour.push_endpoint(p.cvt());
                tracer.stash_multi();
            },
            Op::LineTo { p } => {
                self.current_contour.push_endpoint(p.cvt());
                tracer.stash_multi();
            },
            Op::CurveTo { c1, c2, p } => {
                self.current_contour.push_cubic(c1.cvt(), c2.cvt(), p.cvt());
                tracer.stash_multi();
            },
            Op::Rect { rect } => {
                self.flush();
                self.current_outline.push_contour(Contour::from_rect(rect.cvt()));
                tracer.stash_multi();
            },
            Op::EndPath => {
                self.current_contour.clear();
                self.current_outline.clear();
            }
            Op::Stroke => {
                self.flush();
                self.graphics_state.draw(self.scene, &self.current_outline, DrawMode::Stroke, FillRule::Winding);
                self.trace_outline(tracer);
                self.current_outline.clear();
                tracer.clear();
            },
            Op::FillAndStroke { winding } => {
                self.draw(DrawMode::FillStroke, winding.cvt(), tracer);
            }
            Op::Fill { winding } => {
                self.draw(DrawMode::Fill, winding.cvt(), tracer);
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
                    if let Some(e) = self.cache.get_font(font_ref, self.file)? {
                        debug!("new font: {} at size {}", e.name, size);
                        self.text_state.font_entry = Some(e);
                        self.text_state.font_size = size;
                    } else {
                        self.text_state.font_entry = None;
                    }
                }
            },
            Op::StrokeColor { ref color } => {
                let color = convert_color(&mut self.graphics_state.stroke_color_space, color)?;
                self.graphics_state.set_stroke_color(color);
            },
            Op::FillColor { ref color } => {
                let color = convert_color(&mut self.graphics_state.fill_color_space, color)?;
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
                        self.cache.get_font(font_ref, self.file)?
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
                let mut text_out = String::with_capacity(text.data.len());
                let bb = self.text_state.draw_text(self.scene, &mut self.graphics_state, &text.data, &mut text_out);

                if let (Some(bbox), Some(font_entry)) = (bb.0, self.text_state.font_entry.clone()) {
                    tracer.add_text(TextSpan {
                        bbox,
                        text: text_out,
                        font: font_entry,
                        font_size: self.text_state.font_size * self.text_state.text_matrix.m11() * self.graphics_state.transform.m11()
                    });
                }
                tracer.single(bb);
            },
            Op::TextDrawAdjusted { ref array } => {
                let mut bb = BBox::empty();
                let mut text_out = String::with_capacity(array.len());
                for arg in array {
                    match arg {
                        TextDrawAdjusted::Text(ref data) => {
                            let r2 = self.text_state.draw_text(self.scene, &mut self.graphics_state, data.as_bytes(), &mut text_out);
                            bb.add_bbox(r2);
                        },
                        TextDrawAdjusted::Spacing(offset) => {
                            self.text_state.advance(-0.001 * offset); // because why not PDF…
                        }
                    }
                }
                if let (Some(bbox), Some(font_entry)) = (bb.0, self.text_state.font_entry.clone()) {
                    tracer.add_text(TextSpan {
                        bbox,
                        text: text_out,
                        font: font_entry,
                        font_size: self.text_state.font_size * self.text_state.text_matrix.m11() * self.graphics_state.transform.m11()
                    });
                }
                tracer.single(bb);
            },
            Op::XObject { ref name } => {
                let &xobject_ref = self.resources.xobjects.get(name).unwrap();
                let xobject = self.file.get(xobject_ref)?;
                match *xobject {
                    XObject::Image(_) => {
                        if let &Ok(ref image) = self.cache.get_image(xobject_ref, self.file) {
                            tracer.add_image(&image,
                                self.graphics_state.transform * RectF::new(
                                    Vector2F::new(0.0, 0.0), Vector2F::new(1.0, 1.0)
                                )
                            );
                            let size = image.size();
                            let size_f = size.to_f32();
                            let outline = Outline::from_rect(self.graphics_state.transform * RectF::new(Vector2F::default(), Vector2F::new(1.0, 1.0)));
                            let im_tr = self.graphics_state.transform
                                * Transform2F::from_scale(Vector2F::new(1.0 / size_f.x(), -1.0 / size_f.y()))
                                * Transform2F::from_translation(Vector2F::new(0.0, -size_f.y()));
                            let mut pattern = Pattern::from_image(image.clone());
                            pattern.apply_transform(im_tr);
                            let paint = Paint::from_pattern(pattern);
                            let paint_id = self.scene.push_paint(&paint);
                            let mut draw_path = DrawPath::new(outline, paint_id);
                            draw_path.set_clip_path(self.graphics_state.clip_path_id(self.scene));
                            self.scene.push_draw_path(draw_path);
                    
                            tracer.single(self.graphics_state.transform * RectF::new(Vector2F::default(), size_f));
                            
                        }
                    }
                    XObject::Form(ref content) => {
                        self.draw_form(content, tracer)?;
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
            _ => {}
        }
        match self.resources.color_spaces.get(name) {
            Some(cs) => Ok(cs),
            None => Err(PdfError::Other { msg: format!("color space {:?} not present", name) })
        }
    }
    fn debug_outline(&mut self, outline: Outline, color: ColorU) {
        let paint = self.scene.push_paint(&Paint::from_color(color));
        let mut draw_path = DrawPath::new(outline, paint);
        self.scene.push_draw_path(draw_path);
    }
    fn flush(&mut self) {
        if !self.current_contour.is_empty() {
            self.current_outline.push_contour(self.current_contour.clone());
            self.current_contour.clear();
        }
    }
    fn trace_outline(&self, tracer: &mut Tracer) {
        tracer.multi(self.graphics_state.transform * self.current_outline.bounds());
    }
    fn draw(&mut self, mode: DrawMode, fill_rule: FillRule, tracer: &mut Tracer) {
        self.flush();
        self.graphics_state.draw(self.scene, &self.current_outline, mode, fill_rule);
        self.trace_outline(tracer);
        self.current_outline.clear();
        tracer.clear();
    }
    fn draw_form(&mut self, form: &FormXObject, tracer: &mut Tracer) -> Result<()> {
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
            scene: self.scene,
            cache: self.cache,
            file: self.file,
            items: std::mem::replace(&mut self.items, ItemMap::new()),
        };
        
        for op in form.operations.iter() {
            inner.draw_op(op, tracer)?;
        }
        self.items = inner.items;

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

fn convert_color<'a>(cs: &mut &'a ColorSpace, color: &Color) -> Result<(f32, f32, f32)> {
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
        Color::Other(ref args) => match **cs {
            ColorSpace::DeviceGray => {
                if args.len() != 1 {
                    return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                }
                let g = args[0].as_number()?;
                Ok(gray2rgb(g))
            }
            ColorSpace::DeviceRGB => {
                if args.len() != 3 {
                    return Err(PdfError::Other { msg: format!("expected 3 color arguments, got {:?}", args) });
                }
                let r = args[0].as_number()?;
                let g = args[1].as_number()?;
                let b = args[2].as_number()?;
                Ok((r, g, b))
            }
            ColorSpace::DeviceCMYK => {
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
                //dbg!(args);
                //assert_eq!(args.len(), tint.input_dim());
                //dbg!(tint.output_dim());
                //panic!();
                //tint.apply(args)
                unimplemented!("DeviceN colorspace")
            }
            ColorSpace::Icc(ref icc) => {
                match icc.info.alternate {
                    Some(ref alt) => *cs = &**alt,
                    None => return Err(PdfError::Other { msg: format!("ICC profile without alternate color space") }),
                }
                convert_color(cs, color)
            }
            ColorSpace::Separation(ref _name, ref alt, ref f) => {
                if args.len() != 1 {
                    return Err(PdfError::Other { msg: format!("expected 1 color arguments, got {:?}", args) });
                }
                let x = args[0].as_number()?;
                match &**alt {
                    &ColorSpace::DeviceCMYK => {
                        let mut cmyk = [0.0; 4];
                        f.apply(&[x], &mut cmyk)?;
                        let [c, m, y, k] = cmyk;
                        Ok(cmyk2rgb((c, m, y, k)))
                    },
                    &ColorSpace::DeviceRGB => {
                        let mut rgb = [0.0, 0.0, 0.0];
                        f.apply(&[x], &mut rgb)?;
                        let [r, g, b] = rgb;
                        Ok((r, g, b))
                    },
                    c => unimplemented!("{:?}", c)
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
                    _ => unimplemented!()
                }
            }
            ColorSpace::Other(_) => unimplemented!()
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
