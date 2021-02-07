use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::fs;
use std::borrow::Cow;

use pdf::file::File as PdfFile;
use pdf::object::*;
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

pub type FontMap = HashMap<String, FontEntry>;
pub struct Cache {
    // shared mapping of fontname -> font
    fonts: FontMap,
    op_stats: HashMap<String, (usize, Duration)>,
    standard_fonts: PathBuf,
}
#[derive(Debug)]
pub struct ItemMap(Vec<(RectF, Box<dyn std::fmt::Debug>)>);
impl ItemMap {
    pub fn print(&self, p: Vector2F) {
        for &(rect, ref op) in self.0.iter() {
            if rect.contains_point(p) {
                println!("{:?}", op);
            }
        }
    }
    pub fn get_string(&self, p: Vector2F) -> Option<String> {
        use itertools::Itertools;
        let mut iter = self.0.iter().filter_map(|&(rect, ref op)| {
            if rect.contains_point(p) {
                Some(op)
            } else {
                None
            }
        }).peekable();
        if iter.peek().is_some() {
            Some(format!("{:?}", iter.format(", ")))
        } else {
            None
        }
    }
    pub fn new() -> Self {
        ItemMap(Vec::new())
    }
    pub fn add_rect(&mut self, rect: RectF, item: impl std::fmt::Debug + 'static) {
        self.0.push((rect, Box::new(item) as _));
    }
    pub fn add_bbox(&mut self, bbox: BBox, item: impl std::fmt::Debug + 'static) {
        if let Some(r) = bbox.rect() {
            self.add_rect(r, item);
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

    pub fn render_page<B: Backend>(&mut self, file: &PdfFile<B>, page: &Page, transform: Transform2F) -> Result<(Scene, ItemMap)> {
        let Rect { left, right, top, bottom } = page.media_box(file).expect("no media box");
        let rect = RectF::from_points(Vector2F::new(left, bottom), Vector2F::new(right, top));
        
        let scale = 25.4 / 72.;
        let mut scene = Scene::new();
        let view_box = transform * RectF::new(Vector2F::default(), rect.size() * scale);
        scene.set_view_box(view_box);
        
        let white = scene.push_paint(&Paint::from_color(ColorU::white()));

        scene.push_draw_path(DrawPath::new(Outline::from_rect(view_box), white));

        let mut items = ItemMap::new();

        let root_transformation = transform * Transform2F::from_scale(scale) * Transform2F::row_major(1.0, 0.0, -left, 0.0, -1.0, top);
        
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
        
        for op in contents.operations.iter() {
            let t0 = Instant::now();
            renderstate.draw_op(op)?;
            let dt = t0.elapsed();

            let s = op.operator.as_str();
            let slot = self.op_stats.entry(s.into()).or_default(); 
            slot.0 += 1;
            slot.1 += dt;
        }

        Ok((scene, items))
    }
    pub fn report(&self) {
        let mut ops: Vec<_> = self.op_stats.iter().map(|(name, &(count, duration))| (count, name.as_str(), duration)).collect();
        ops.sort_unstable();

        for (count, name, duration) in ops {
            println!("{:6}  {:5}  {}ms", count, name, duration.as_millis());
        }
    }
}
