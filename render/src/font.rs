use std::path::{Path, PathBuf};
use std::ops::Deref;
use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use font::{self};
use std::sync::Arc;
use super::FontEntry;
use cachelib::{sync::SyncCache, ValueSize};
use std::hash::{Hash, Hasher};
use once_cell::sync::OnceCell;

static STANDARD_FONTS: OnceCell<Vec<(String, String)>> = OnceCell::new();
pub fn standard_fonts(dir: &Path) -> &[(String, String)] {
    STANDARD_FONTS.get_or_init(|| {
        let data = std::fs::read_to_string(dir.join("fonts.json")).expect("can't read fonts.json");
        let fonts: Vec<(String, String)> = serde_json::from_str(&data).expect("fonts.json is invalid");
        fonts
    })
}

#[derive(Clone)]
pub struct FontRc(Arc<dyn font::Font + Send + Sync + 'static>);
impl ValueSize for FontRc {
    #[inline]
    fn size(&self) -> usize {
        1 // TODO
    }
}
impl From<Box<dyn font::Font + Send + Sync + 'static>> for FontRc {
    #[inline]
    fn from(f: Box<dyn font::Font + Send + Sync + 'static>) -> Self {
        FontRc(f.into())
    }
}
impl Deref for FontRc {
    type Target = dyn font::Font + Send + Sync + 'static;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
impl PartialEq for FontRc {
    #[inline]
    fn eq(&self, rhs: &Self) -> bool {
        Arc::as_ptr(&self.0) == Arc::as_ptr(&rhs.0)
    }
}
impl Eq for FontRc {}
impl Hash for FontRc {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state)
    }
}
pub struct StandardCache {
    inner: Arc<SyncCache<String, Option<FontRc>>>
}
impl StandardCache {
    pub fn new() -> Self {
        StandardCache { inner: SyncCache::new() }
    }
}

pub fn load_font(font_ref: Ref<PdfFont>, resolve: &impl Resolve, standard_fonts_dir: &Path, cache: &StandardCache) -> Result<Option<FontEntry>> {
    let pdf_font = resolve.get(font_ref)?;
    debug!("loading {:?}", pdf_font);
    
    let font: FontRc = match pdf_font.embedded_data(resolve) {
        Some(Ok(data)) => {
            let font = font::parse(&data).map_err(|e| {
                let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
                std::fs::write(&name, &data).unwrap();
                println!("font dumped in {}", name);
                PdfError::Other { msg: format!("Font Error: {:?}", e) }
            })?;
            FontRc::from(font)
        }
        Some(Err(e)) => return Err(e),
        None => {
            match standard_fonts(standard_fonts_dir).iter().find(|&(ref name, _)| pdf_font.name.as_ref().map(|s| s == name.as_str()).unwrap_or(false)) {
                Some(&(_, ref file_name)) => {
                    let val = cache.inner.get(file_name.clone(), || {
                        let data = match std::fs::read(standard_fonts_dir.join(file_name)) {
                            Ok(data) => data,
                            Err(e) => {
                                warn!("can't open {} for {:?} {:?}", file_name, pdf_font.name, e);
                                return None;
                            }
                        };
                        match font::parse(&data) {
                            Ok(f) => Some(f.into()),
                            Err(e) => {
                                warn!("Font Error: {:?}", e);
                                return None;
                            }
                        }
                    });
                    match val {
                        Some(f) => f,
                        None => {
                            return Ok(None);
                        }
                    }
                }
                None => {
                    warn!("no font for {:?}", pdf_font.name);
                    return Ok(None);
                }
            }
        }
    };

    Ok(Some(FontEntry::build(font, pdf_font, resolve)?))
}
