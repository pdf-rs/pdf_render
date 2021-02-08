use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::fs;
use std::borrow::Cow;

use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::content::Operation;
use pdf::backend::Backend;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};

use pathfinder_geometry::{
    vector::Vector2F,
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_color::ColorU;
use pathfinder_renderer::{
    scene::{DrawPath, Scene},
    paint::Paint,
};
use pathfinder_content::outline::Outline;
use font::{self};

use super::{BBox, STANDARD_FONTS, fontentry::FontEntry, renderstate::RenderState};

use std::time::{Duration, Instant};

const SCALE: f32 = 25.4 / 72.;

pub type FontMap = HashMap<String, FontEntry>;
pub struct Cache {
    // shared mapping of fontname -> font
    fonts: FontMap,
    op_stats: HashMap<String, (usize, Duration)>,
    standard_fonts: PathBuf,
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
            standard_fonts: std::env::var_os("STANDARD_FONTS").map(PathBuf::from).unwrap_or(PathBuf::from("fonts"))
        }
    }
    fn load_font(&mut self, pdf_font: &PdfFont) {
        if self.fonts.get(&pdf_font.name).is_some() {
            return;
        }
        
        debug!("loading {:?}", pdf_font);
        
        let data: Cow<[u8]> = match pdf_font.embedded_data() {
            Some(Ok(data)) => {
                if let Some(path) = std::env::var_os("PDF_FONTS") {
                    let file = PathBuf::from(path).join(&pdf_font.name);
                    fs::write(file, data).expect("can't write font");
                }
                data.into()
            }
            Some(Err(e)) => panic!("can't decode font data: {:?}", e),
            None => {
                match STANDARD_FONTS.iter().find(|&&(name, _)| pdf_font.name == name) {
                    Some(&(_, file_name)) => {
                        if let Ok(data) = std::fs::read(self.standard_fonts.join(file_name)) {
                            data.into()
                        } else {
                            warn!("can't open {} for {}", file_name, pdf_font.name);
                            return;
                        }
                    }
                    None => {
                        warn!("no font for {}", pdf_font.name);
                        return;
                    }
                }
            }
        };
        let entry = FontEntry::build(font::parse(&data), pdf_font);
        debug!("is_cid={}", entry.is_cid);
        
        self.fonts.insert(pdf_font.name.clone(), entry);
    }

    pub fn page_bounds<B: Backend>(&self, file: &PdfFile<B>, page: &Page) -> RectF {
        let Rect { left, right, top, bottom } = page.media_box(file).expect("no media box");
        RectF::from_points(Vector2F::new(left, bottom), Vector2F::new(right, top)) * SCALE
    }
    pub fn render_page<B: Backend>(&mut self, file: &PdfFile<B>, page: &Page, transform: Transform2F) -> Result<(Scene, ItemMap)> {
        let mut scene = Scene::new();
        let bounds = self.page_bounds(file, page);
        let view_box = transform * bounds;
        scene.set_view_box(view_box);
        
        let white = scene.push_paint(&Paint::from_color(ColorU::white()));
        scene.push_draw_path(DrawPath::new(Outline::from_rect(view_box), white));

        let root_transformation = transform * Transform2F::row_major(SCALE, 0.0, -bounds.min_x(), 0.0, -SCALE, bounds.max_y());
        
        let resources = page.resources(file)?;
        // make sure all fonts are in the cache, so we can reference them
        for font in resources.fonts.values() {
            self.load_font(font);
        }
        for gs in resources.graphics_states.values() {
            if let Some((ref font, _)) = gs.font {
                self.load_font(font);
            }
        }

        let contents = try_opt!(page.contents.as_ref());
        let mut renderstate = RenderState::new(&mut scene, &self.fonts, file, &resources, root_transformation);
        let mut tracer = Tracer {
            nr: 0,
            ops: &contents.operations,
            stash: vec![],
            map: ItemMap::new()
        };
        for (nr, op) in contents.operations.iter().enumerate() {
            tracer.nr = nr;
            let t0 = Instant::now();
            debug!("{:3} {}", nr, op);
            renderstate.draw_op(op, &mut tracer)?;
            let dt = t0.elapsed();

            let s = op.operator.as_str();
            let slot = self.op_stats.entry(s.into()).or_default(); 
            slot.0 += 1;
            slot.1 += dt;
        }

        Ok((scene, tracer.map))
    }
    pub fn report(&self) {
        let mut ops: Vec<_> = self.op_stats.iter().map(|(name, &(count, duration))| (count, name.as_str(), duration)).collect();
        ops.sort_unstable();

        for (count, name, duration) in ops {
            println!("{:6}  {:5}  {}ms", count, name, duration.as_millis());
        }
    }
}

pub struct Tracer<'a> {
    nr: usize,
    ops: &'a [Operation],
    stash: Vec<usize>,
    map: ItemMap,
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
}

#[derive(Debug)]
pub enum TraceItem {
    Single(usize, Operation),
    Multi(Vec<(usize, Operation)>)
}