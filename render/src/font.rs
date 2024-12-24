use std::borrow::Cow;
use std::path::{PathBuf};
use std::ops::Deref;
use std::collections::HashMap;
use glyphmatcher::FontDb;
use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use font::{self};
use std::sync::Arc;
use super::FontEntry;
use globalcache::{sync::SyncCache, ValueSize};
use std::hash::{Hash, Hasher};

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
    inner: Arc<SyncCache<String, Option<FontRc>>>,

    #[cfg(not(feature="embed"))]
    dir: PathBuf,

    #[cfg(feature="embed")]
    dir: EmbeddedStandardFonts,

    fonts: HashMap<String, String>,
    dump: Dump,
    require_unique_unicode: bool,
}
impl StandardCache {
    #[cfg(not(feature="embed"))]
    pub fn new() -> StandardCache {
        let standard_fonts = PathBuf::from(std::env::var_os("STANDARD_FONTS").expect("STANDARD_FONTS is not set. Please check https://github.com/pdf-rs/pdf_render/#fonts for instructions."));

        let data = standard_fonts.read_file("fonts.json").expect("can't read fonts.json");
        let fonts: HashMap<String, String> = serde_json::from_slice(&data).expect("fonts.json is invalid");

        let dump = match std::env::var("DUMP_FONT").as_deref() {
            Err(_) => Dump::Never,
            Ok("always") => Dump::Always,
            Ok("error") => Dump::OnError,
            Ok(_) => Dump::Never
        };
        StandardCache {
            inner: SyncCache::new(),
            dir: standard_fonts,
            fonts,
            dump,
            require_unique_unicode: false,
        }
    }
    #[cfg(feature="embed")]
    pub fn new() -> StandardCache {
        let ref data = EmbeddedStandardFonts::get("fonts.json").unwrap().data;
        let fonts: HashMap<String, String> = serde_json::from_slice(&data).expect("fonts.json is invalid");

        StandardCache {
            inner: SyncCache::new(),
            fonts,
            dir: EmbeddedStandardFonts,
            dump: Dump::Never,
            require_unique_unicode: false,
        }
    }

    pub fn require_unique_unicode(&mut self, r: bool) {
        self.require_unique_unicode = r;
    }
}

pub trait DirRead: Sized {
    fn read_file(&self, name: &str) -> Result<Cow<'static, [u8]>>;
    fn sub_dir(&self, name: &str) -> Option<Self>;
}

impl DirRead for PathBuf {
    fn read_file(&self, name: &str) -> Result<Cow<'static, [u8]>> {
        std::fs::read(self.join(name)).map_err(|e| e.into()).map(|d| d.into())
    }
    fn sub_dir(&self, name: &str) -> Option<Self> {
        let sub = self.join(name);
        if sub.is_dir() {
            Some(sub)
        } else {
            None
        }
    }
}

#[cfg(feature="embed")]
#[derive(rust_embed::Embed)]
#[folder = "$STANDARD_FONTS"]
pub struct EmbeddedStandardFonts;

#[cfg(feature="embed")]
impl DirRead for EmbeddedStandardFonts {
    fn read_file(&self, name: &str) -> Result<Cow<'static, [u8]>> {
        EmbeddedStandardFonts::get(name).map(|f| f.data).ok_or_else(|| PdfError::Other { msg: "Filed {name:?} not embedded".into() })
    }
    fn sub_dir(&self, name: &str) -> Option<Self> {
        None
    }
}

#[derive(Debug)]
enum Dump {
    Never,
    OnError,
    Always
}

pub fn load_font(font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve, cache: &StandardCache) -> Result<Option<FontEntry>> {
    let pdf_font = font_ref.clone();
    debug!("loading {:?}", pdf_font);
    
    let font: FontRc = match pdf_font.embedded_data(resolve) {
        Some(Ok(data)) => {
            debug!("loading embedded font");
            let font = font::parse(&data).map_err(|e| {
                PdfError::Other { msg: format!("Font Error: {:?}", e) }
            });
            if matches!(cache.dump, Dump::Always) || (matches!(cache.dump, Dump::OnError) && font.is_err()) {
                let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
                std::fs::write(&name, &data).unwrap();
                println!("font dumped in {}", name);
            }
            FontRc::from(font?)
        }
        Some(Err(e)) => return Err(e),
        None => {
            debug!("no embedded font.");
            let name = match pdf_font.name {
                Some(ref name) => name.as_str(),
                None => return Ok(None)
            };
            debug!("loading {name} instead");
            match cache.fonts.get(name).or_else(|| cache.fonts.get("Arial")) {
                Some(file_name) => {
                    let val = cache.inner.get(file_name.clone(), |_| {
                        let data = match cache.dir.read_file(file_name) {
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

    Ok(Some(FontEntry::build(font, pdf_font, None, resolve, cache.require_unique_unicode)?))
}
