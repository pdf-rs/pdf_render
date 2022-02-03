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
    data_dir: Option<PathBuf>,
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
        let standard_fonts;
        if let Some(path) = std::env::var_os("STANDARD_FONTS") {
            standard_fonts = PathBuf::from(path);
        } else {
            eprintln!("PDF: STANDARD_FONTS not set. using fonts/ instead.");
            standard_fonts = PathBuf::from("fonts");
        }
        if !standard_fonts.is_dir() {
            panic!("STANDARD_FONTS (or fonts/) is not directory.");
        }
        Cache {
            fonts: HashMap::new(),
            op_stats: HashMap::new(),
            images: HashMap::new(),
            standard_fonts,
            data_dir: None,
        }
    }
    pub fn clear_image_cache(&mut self) {
        self.images.clear();
    }
    pub fn set_data_dir(&mut self, dir: &Path) {
        self.data_dir = Some(dir.into());
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
                    if let Some(ref name) = pdf_font.name {
                        let file = PathBuf::from(path).join(name);
                        fs::write(file, data).expect("can't write font");
                    }
                }
                data.into()
            }
            Some(Err(e)) => return Err(e),
            None => {
                match STANDARD_FONTS.iter().find(|&&(name, _)| pdf_font.name.as_ref().map(|s| s == name).unwrap_or(false)) {
                    Some(&(_, file_name)) => {
                        if let Ok(data) = std::fs::read(standard_fonts.join(file_name)) {
                            data.into()
                        } else {
                            warn!("can't open {} for {:?}", file_name, pdf_font.name);
                            return Ok(None);
                        }
                    }
                    None => {
                        warn!("no font for {:?}", pdf_font.name);
                        return Ok(None);
                    }
                }
            }
        };

        let font = font::parse(&data).map_err(|e| {
            let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
            std::fs::write(&name, &data).unwrap();
            println!("font dumped in {}", name);
            PdfError::Other { msg: format!("Font Error: {:?}", e) }
        })?;
        let entry = match FontEntry::build(font, pdf_font, resolve) {
            Ok(e) => Rc::new(e),
            Err(e) => {
                info!("Failed to build FontEntry: {:?}", e);
                return Ok(None);
            }
        };
        debug!("is_cid={}", entry.is_cid);
        
        Ok(Some(entry))
    }

    pub fn get_image(&mut self, xobject_ref: Ref<XObject>, resolve: &impl Resolve) -> &Result<Image> {
        match self.images.entry(xobject_ref) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let im = Self::load_image(xobject_ref, resolve, &self.data_dir);
                if let Err(ref e) = im {
                    dbg!(e);
                }
                e.insert(im)
            }
        }
    }

    fn load_image(xobject_ref: Ref<XObject>, resolve: &impl Resolve, data: &Option<PathBuf>) -> Result<Image> {
        let xobject = t!(resolve.get(xobject_ref));
        match *xobject {
            XObject::Image(ref image) => {
                let raw_data = match image.decode() {
                    Ok(i) => i,
                    Err(e) => {
                        if let Some(ref dir) = data {
                            std::fs::create_dir_all(dir).unwrap();
                            let data_name = format!("img_{}.data", xobject_ref.get_inner().id);
                            std::fs::write(dir.join(data_name), image.raw_data()).unwrap();
                            let info = format!("{:?}", image.info);
                            let info_name = format!("img_{}.txt", xobject_ref.get_inner().id);
                            std::fs::write(dir.join(info_name), &info).unwrap();
                        }
                        return Err(e);
                    }
                };
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

                fn resolve_cs(cs: &ColorSpace) -> Option<&ColorSpace> {
                    match cs {
                        ColorSpace::Icc(icc) => icc.info.info.alternate.as_ref().map(|b| &**b),
                        _ => Some(cs),
                    }
                }
                let cs = image.color_space.as_ref().and_then(resolve_cs);
                
                let data_ratio = raw_data.len() / pixel_count;
                let data = match data_ratio {
                    1 => match cs {
                        Some(ColorSpace::DeviceGray) => {
                            assert_eq!(raw_data.len(), pixel_count);
                            raw_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
                        }
                        Some(ColorSpace::Indexed(ref base, ref lookup)) => {
                            match resolve_cs(&**base) {
                                Some(ColorSpace::DeviceRGB) => {
                                    raw_data.iter().zip(alpha).map(|(&b, a)| {
                                        let off = b as usize * 3;
                                        let c = lookup.get(off .. off + 3).unwrap_or(&[0; 3]);
                                        ColorU { r: c[0], g: c[1], b: c[2], a }
                                    }).collect()
                                }
                                Some(ColorSpace::DeviceCMYK) => {
                                    raw_data.iter().zip(alpha).map(|(&b, a)| {
                                        let off = b as usize * 4;
                                        let c = lookup.get(off .. off + 4).unwrap_or(&[0; 4]);
                                        cmyk2color(c.try_into().unwrap(), a)
                                    }).collect()
                                }
                                _ => unimplemented!("base cs={:?}", base),
                            }
                        }
                        Some(ColorSpace::Separation(_, ref alt, ref func)) => {
                            let mut lut = [[0u8; 3]; 256];

                            match resolve_cs(alt) {
                                Some(ColorSpace::DeviceRGB) => {
                                    for (i, rgb) in lut.iter_mut().enumerate() {
                                        let mut c = [0.; 3];
                                        func.apply(&[i as f32 / 255.], &mut c)?;
                                        let [r, g, b] = c;
                                        *rgb = [(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8];
                                    }
                                }
                                Some(ColorSpace::DeviceCMYK) => {
                                    for (i, rgb) in lut.iter_mut().enumerate() {
                                        let mut c = [0.; 4];
                                        func.apply(&[i as f32 / 255.], &mut c)?;
                                        let [c, m, y, k] = c;
                                        *rgb = cmyk2rgb([(c * 255.) as u8, (m * 255.) as u8, (y * 255.) as u8, (k * 255.) as u8]);
                                    }
                                }
                                _ => unimplemented!("alt cs={:?}", alt),
                            }
                            raw_data.iter().zip(alpha).map(|(&b, a)| {
                                let [r, g, b] = lut[b as usize];
                                ColorU { r, g, b, a }
                            }).collect()
                        }
                        _ => unimplemented!("cs={:?}", cs),
                    }
                    3 => {
                        if !matches!(cs, Some(ColorSpace::DeviceRGB)) {
                            info!("image has data/pixel ratio of 3, but colorspace is {:?}", cs);
                        }
                        raw_data.chunks_exact(3).zip(alpha).map(|(c, a)| ColorU { r: c[0], g: c[1], b: c[2], a }).collect()
                    }
                    4 => {
                        if !matches!(cs, Some(ColorSpace::DeviceCMYK)) {
                            info!("image has data/pixel ratio of 4, but colorspace is {:?}", cs);
                        }
                        cmyk2color_arr(&raw_data, alpha)
                    }
                    _ => unimplemented!("data/pixel ratio {}", data_ratio),
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
        let rotate = Transform2F::from_rotation(page.rotate as f32 * std::f32::consts::PI / 180.);
        let br = rotate * bounds;
        let translate = Transform2F::from_translation(Vector2F::new(
            -br.min_x().min(br.max_x()),
            -br.min_y().min(br.max_y()),
        ));
        let view_box = transform * translate * rotate * bounds;
        scene.set_view_box(view_box);
        
        let white = scene.push_paint(&Paint::from_color(ColorU::white()));
        scene.push_draw_path(DrawPath::new(Outline::from_rect(view_box), white));

        let root_transformation = transform
            * translate
            * rotate
            * Transform2F::row_major(SCALE, 0.0, -bounds.min_x(), 0.0, -SCALE, bounds.max_y());
        
        let resources = t!(page.resources());

        let contents = try_opt!(page.contents.as_ref());
        let ops = contents.operations(file)?;
        let mut renderstate = RenderState::new(&mut scene, self, file, &resources, root_transformation);
        let mut tracer = Tracer {
            nr: 0,
            ops,
            stash: vec![],
            map: ItemMap::new(),
            draw: Vec::new(),
        };
        for (i, op) in ops.iter().enumerate().take(limit.unwrap_or(usize::MAX)) {
            debug!("op {}: {:?}", i, op);
            tracer.nr = i;
            renderstate.draw_op(op, &mut tracer)?;
        }

        let results = TraceResults {
            items: tracer.map,
            draw: tracer.draw,
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
    // A rect with the origin at the baseline, a height of 1em and width that corresponds to the advance width.
    pub rect: RectF,

    // width in textspace units (before applying transform)
    pub width: f32,
    // Bounding box of the rendered outline
    pub bbox: RectF,
    pub font_size: f32,
    pub font: Rc<FontEntry>,
    pub text: String,
    pub color: ColorU,

    // apply this transform to a text draw in at the origin with the given width and font-size
    pub transform: Transform2F,
}

pub struct ImageObject {
    pub data: Arc<Vec<ColorU>>,
    pub size: (u32, u32),
    pub rect: RectF,
    pub id: PlainRef,
}
impl ImageObject {
    pub fn rgba_data(&self) -> &[u8] {
        let ptr: *const ColorU = self.data.as_ptr();
        let len = self.data.len();
        unsafe {
            std::slice::from_raw_parts(ptr.cast(), 4 * len)
        }
    }
}

pub struct TraceResults {
    pub items: ItemMap,
    pub draw: Vec<DrawItem>,
}
impl TraceResults {
    pub fn texts(&self) -> impl Iterator<Item=&TextSpan> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Text(t) => Some(t),
            _ => None
        })
    }
    pub fn images(&self) -> impl Iterator<Item=&ImageObject> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Image(i) => Some(i),
            _ => None
        })
    }
    pub fn paths(&self) -> impl Iterator<Item=&VectorPath> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Vector(p) => Some(p),
            _ => None
        })
    }
}
pub enum DrawItem {
    Vector(VectorPath),
    Image(ImageObject),
    Text(TextSpan),
}

#[derive(Debug)]
pub struct VectorPath {
    pub outline: Outline,
    pub fill: Option<ColorU>,
    pub stroke: Option<(ColorU, f32)>,
}

pub struct Tracer<'a> {
    nr: usize,
    ops: &'a [Op],
    stash: Vec<usize>,
    map: ItemMap,
    draw: Vec<DrawItem>,
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
        self.draw.push(DrawItem::Text(span));
    }
    pub fn add_image(&mut self, image: &Image, rect: RectF, id: PlainRef) {
        self.draw.push(DrawItem::Image(ImageObject {
            data: image.pixels().clone(),
            size: (image.size().x() as u32, image.size().y() as u32),
            rect,
            id
        }));
    }
    pub fn add_path(&mut self, path: VectorPath) {
        self.draw.push(DrawItem::Vector(path));
    }
}

#[derive(Debug)]
pub enum TraceItem {
    Single(usize, Op),
    Multi(Vec<(usize, Op)>)
}

fn cmyk2rgb([c, m, y, k]: [u8; 4]) -> [u8; 3] {
    let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
    let r = 255 - c.saturating_add(k);
    let g = 255 - m.saturating_add(k);
    let b = 255 - y.saturating_add(k);
    [r, g, b]
}
fn cmyk2color([c, m, y, k]: [u8; 4], a: u8) -> ColorU {
    let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
    let r = 255 - c.saturating_add(k);
    let g = 255 - m.saturating_add(k);
    let b = 255 - y.saturating_add(k);
    ColorU::new(r, g, b, a)
}

fn cmyk2color_arr(data: &[u8], alpha: impl Iterator<Item=u8>) -> Vec<ColorU> {
    data.chunks_exact(4).zip(alpha).map(|(c, a)| {
        let mut buf = [0; 4];
        buf.copy_from_slice(c);
        cmyk2color(buf, a)
    }).collect()
}
