use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::borrow::Cow;
use std::sync::Arc;

use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::content::Op;
use pdf::backend::Backend;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use pathfinder_geometry::{
    vector::{Vector2F, Vector2I},
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_color::ColorU;
use pathfinder_renderer::{
    scene::{DrawPath, Scene},
    paint::Paint,
};
use pathfinder_content::{
    outline::Outline,
    pattern::{Image},
};
use font::{self};
use std::rc::Rc;

use super::{BBox, STANDARD_FONTS, fontentry::FontEntry, renderstate::RenderState};

use instant::{Duration};

const SCALE: f32 = 25.4 / 72.;


pub type FontMap = HashMap<Ref<PdfFont>, Option<Rc<FontEntry>>>;
pub type ImageMap = HashMap<Ref<XObject>, Result<Image>>;
pub struct Cache {
    // shared mapping of fontname -> font
    fonts: FontMap,
    op_stats: HashMap<String, (usize, Duration)>,
    standard_fonts: PathBuf,
    images: ImageMap,
}
#[derive(Debug)]
pub struct ItemMap(Vec<(RectF, TraceItem)>);
impl ItemMap {
    pub fn matches(&self, p: Vector2F) -> impl Iterator<Item=&TraceItem> + '_ {
        self.0.iter()
            .filter(move |&(rect, _)| rect.contains_point(p))
            .map(|&(_, ref item)| item)
    }
    pub fn new() -> Self {
        ItemMap(Vec::new())
    }
    pub fn add(&mut self, bbox: BBox, item: TraceItem) {
        if let Some(r) = bbox.rect() {
            self.0.push((r, item));
        }
    }
}
impl Cache {
    pub fn new() -> Cache {
        Cache {
            fonts: HashMap::new(),
            op_stats: HashMap::new(),
            images: HashMap::new(),
            standard_fonts: std::env::var_os("STANDARD_FONTS").map(PathBuf::from).unwrap_or(PathBuf::from("fonts"))
        }
    }
    pub fn get_font(&mut self, font_ref: Ref<PdfFont>, resolve: &impl Resolve) -> Result<Option<Rc<FontEntry>>> {
        match self.fonts.entry(font_ref) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let font = Self::load_font(font_ref, resolve, &self.standard_fonts)?;
                Ok(e.insert(font).clone())
            }
        }
    }
    fn load_font(font_ref: Ref<PdfFont>, resolve: &impl Resolve, standard_fonts: &Path) -> Result<Option<Rc<FontEntry>>> {
        let pdf_font = resolve.get(font_ref)?;
        debug!("loading {:?}", pdf_font);
        
        let data: Cow<[u8]> = match pdf_font.embedded_data() {
            Some(Ok(data)) => {
                if let Some(path) = std::env::var_os("PDF_FONTS") {
                    let file = PathBuf::from(path).join(&pdf_font.name);
                    fs::write(file, data).expect("can't write font");
                }
                data.into()
            }
            Some(Err(e)) => return Err(e),
            None => {
                match STANDARD_FONTS.iter().find(|&&(name, _)| pdf_font.name == name) {
                    Some(&(_, file_name)) => {
                        if let Ok(data) = std::fs::read(standard_fonts.join(file_name)) {
                            data.into()
                        } else {
                            warn!("can't open {} for {}", file_name, pdf_font.name);
                            return Ok(None);
                        }
                    }
                    None => {
                        warn!("no font for {}", pdf_font.name);
                        return Ok(None);
                    }
                }
            }
        };
        let entry = Rc::new(FontEntry::build(font::parse(&data), &pdf_font, resolve)?);
        debug!("is_cid={}", entry.is_cid);
        
        Ok(Some(entry))
    }

    pub fn get_image(&mut self, xobject_ref: Ref<XObject>, resolve: &impl Resolve) -> &Result<Image> {
        match self.images.entry(xobject_ref) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let im = Self::load_image(xobject_ref, resolve);
                e.insert(im)
            }
        }
    }

    fn load_image(xobject_ref: Ref<XObject>, resolve: &impl Resolve) -> Result<Image> {
        let xobject = t!(resolve.get(xobject_ref));
        match *xobject {
            XObject::Image(ref image) => {
                let raw_data = t!(image.data());
                let pixel_count = image.width as usize * image.height as usize;
                if raw_data.len() % pixel_count != 0 {
                    warn!("invalid data length {} bytes for {} pixels", raw_data.len(), pixel_count);
                    return Err(PdfError::Other { msg: format!("image data is {} (not a multiple of {}).", raw_data.len(), pixel_count)});
                }
                info!("smask: {:?}", image.smask);

                let mask = t!(image.smask.map(|r| resolve.get(r)).transpose());
                let alpha = match mask {
                    Some(ref mask) => t!(mask.data()),
                    None => &[]
                };
                let alpha = alpha.iter().cloned().chain(std::iter::repeat(255));


                let data = match image.color_space {
                    Some(ColorSpace::DeviceRGB) => {
                        assert_eq!(raw_data.len(), pixel_count * 3);
                        raw_data.chunks_exact(3).zip(alpha).map(|(c, a)| ColorU { r: c[0], g: c[1], b: c[2], a }).collect()
                    }
                    Some(ColorSpace::DeviceCMYK) => {
                        assert_eq!(raw_data.len(), pixel_count * 4);
                        cmyk2color(raw_data, alpha)
                    }
                    Some(ColorSpace::DeviceGray) => {
                        assert_eq!(raw_data.len(), pixel_count);
                        raw_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
                    }
                    Some(ColorSpace::DeviceN { ref tint, .. }) => unimplemented!("DeviceN colorspace"),
                    /*{
                        let components = raw_data.len() / pixel_count;
                        assert_eq!(components, tint.input_dim());
                        dbg!(tint.output_dim());

                        for c in raw_data.chunks_exact(components) {}
                        panic!()
                    }*/
                    //Some(ColorSpace::Indexed(ref base, ref lookup)) => panic!(),
                    ref cs => unimplemented!("cs={:?}", cs),
                };

                let size = Vector2I::new(image.width as _, image.height as _);
                Ok(Image::new(size, Arc::new(data)))
            }
            _ => Err(PdfError::Other { msg: "not an image".into() })
        }
    }
    pub fn page_bounds<B: Backend>(&self, file: &PdfFile<B>, page: &Page) -> RectF {
        let Rect { left, right, top, bottom } = page.media_box().expect("no media box");
        RectF::from_points(Vector2F::new(left, bottom), Vector2F::new(right, top)) * SCALE
    }
    pub fn render_page<B: Backend>(&mut self, file: &PdfFile<B>, page: &Page, transform: Transform2F) -> Result<(Scene, TraceResults)> {
        self.render_page_limited(file, page, transform, None)
    }
    pub fn render_page_limited<B: Backend>(&mut self, file: &PdfFile<B>, page: &Page, transform: Transform2F, limit: Option<usize>) -> Result<(Scene, TraceResults)> {
        let mut scene = Scene::new();
        let bounds = self.page_bounds(file, page);
        let view_box = transform * bounds;
        scene.set_view_box(view_box);
        
        let white = scene.push_paint(&Paint::from_color(ColorU::white()));
        scene.push_draw_path(DrawPath::new(Outline::from_rect(view_box), white));

        let root_transformation = transform * Transform2F::row_major(SCALE, 0.0, -bounds.min_x(), 0.0, -SCALE, bounds.max_y());
        
        let resources = t!(page.resources());

        let contents = try_opt!(page.contents.as_ref());
        let ops = contents.operations(file)?;
        let mut renderstate = RenderState::new(&mut scene, self, file, &resources, root_transformation);
        let mut tracer = Tracer {
            nr: 0,
            ops,
            stash: vec![],
            map: ItemMap::new(),
            text: Vec::new(),
            images: Vec::new(),
        };
        for (i, op) in ops.iter().enumerate().take(limit.unwrap_or(usize::MAX)) {
            debug!("op {}: {:?}", i, op);
            tracer.nr = i;
            renderstate.draw_op(op, &mut tracer)?;
        }

        let results = TraceResults {
            items: tracer.map,
            text: tracer.text,
            images: tracer.images,
        };
        Ok((scene, results))
    }
    pub fn report(&self) {
        let mut ops: Vec<_> = self.op_stats.iter().map(|(name, &(count, duration))| (count, name.as_str(), duration)).collect();
        ops.sort_unstable();

        for (count, name, duration) in ops {
            println!("{:6}  {:5}  {}ms", count, name, duration.as_millis());
        }
    }
}

pub struct TextSpan {
    pub bbox: RectF,
    pub font_size: f32,
    pub font: Rc<FontEntry>,
    pub text: String,
}

pub struct ImageObject {
    pub data: Arc<Vec<ColorU>>,
    pub size: (u32, u32),
    pub rect: RectF,
}

pub struct TraceResults {
    pub items: ItemMap,
    pub text: Vec<TextSpan>,
    pub images: Vec<ImageObject>
}

pub struct Tracer<'a> {
    nr: usize,
    ops: &'a [Op],
    stash: Vec<usize>,
    map: ItemMap,
    text: Vec<TextSpan>,
    images: Vec<ImageObject>,
}
impl<'a> Tracer<'a> {
    pub fn single(&mut self, bb: impl Into<BBox>) {
        self.map.add(bb.into(), TraceItem::Single(self.nr, self.ops[self.nr].clone()));
    }
    pub fn stash_multi(&mut self) {
        self.stash.push(self.nr);
    }
    pub fn multi(&mut self, bb: impl Into<BBox>) {
        self.stash.push(self.nr);
        self.map.add(bb.into(), TraceItem::Multi(self.stash.iter().map(|&n| (n, self.ops[n].clone())).collect()));
    }
    pub fn clear(&mut self) {
        self.stash.clear();
    }
    pub fn nr(&self) -> usize {
        self.nr
    }
    pub fn add_text(&mut self, span: TextSpan) {
        self.text.push(span);
    }
    pub fn add_image(&mut self, image: &Image, rect: RectF) {
        self.images.push(ImageObject {
            data: image.pixels().clone(),
            size: (image.size().x() as u32, image.size().y() as u32),
            rect
        })
    }
}

#[derive(Debug)]
pub enum TraceItem {
    Single(usize, Op),
    Multi(Vec<(usize, Op)>)
}

fn cmyk2color(data: &[u8], alpha: impl Iterator<Item=u8>) -> Vec<ColorU> {
    data.chunks_exact(4).zip(alpha).map(|(c, a)| {
        let mut buf = [0; 4];
        buf.copy_from_slice(c);

        let [c, m, y, k] = buf;
        let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
        let r = 255 - c.saturating_add(k);
        let g = 255 - m.saturating_add(k);
        let b = 255 - y.saturating_add(k);
        ColorU::new(r, g, b, a)
    }).collect()
}
