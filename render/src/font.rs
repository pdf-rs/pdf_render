use std::path::{PathBuf};
use std::ops::Deref;
use std::collections::HashMap;

#[cfg(feature="glyphmatcher")]
use glyphmatcher::FontDb;

use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use font::{self, Encoder, FontType, FontVariant};
use vello_encoding::{Encoding, PathEncoder};
use std::sync::Arc;
use super::FontEntry;
use globalcache::{sync::SyncCache, ValueSize};
use std::hash::{Hash, Hasher};

pub struct FontRc<E: Encoder>(Arc<font::FontVariant<E>>);
impl<E: Encoder> Clone for FontRc<E> {
    #[inline]
    fn clone(&self) -> Self {
        FontRc(self.0.clone())
    }
}
impl<E: Encoder> ValueSize for FontRc<E> {
    #[inline]
    fn size(&self) -> usize {
        1 // TODO
    }
}
impl<E: Encoder> From<font::FontVariant<E>> for FontRc<E> {
    #[inline]
    fn from(f: font::FontVariant<E>) -> Self {
        FontRc(f.into())
    }
}
impl<E: Encoder> Deref for FontRc<E> {
    type Target = FontVariant<E>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
impl<E: Encoder> PartialEq for FontRc<E> {
    #[inline]
    fn eq(&self, rhs: &Self) -> bool {
        Arc::as_ptr(&self.0) == Arc::as_ptr(&rhs.0)
    }
}
impl<E: Encoder> Eq for FontRc<E> {}
impl<E: Encoder> Hash for FontRc<E> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state)
    }
}
pub struct StandardCache<E: Encoder> {
    inner: Arc<SyncCache<String, Option<FontRc<E>>>>,
    dir: PathBuf,
    fonts: HashMap<String, String>,
    dump: Dump,

    #[cfg(feature="glyphmatcher")]
    font_db: Option<FontDb>,
    
    require_unique_unicode: bool,
}
impl<E: Encoder + 'static> StandardCache<E> where E::GlyphRef: Sync + Send {
    pub fn new(dir: PathBuf) -> Self {
        let data = std::fs::read_to_string(dir.join("fonts.json")).expect("can't read fonts.json");
        let fonts: HashMap<String, String> = serde_json::from_str(&data).expect("fonts.json is invalid");

        let dump = match std::env::var("DUMP_FONT").as_deref() {
            Err(_) => Dump::Never,
            Ok("always") => Dump::Always,
            Ok("error") => Dump::OnError,
            Ok(_) => Dump::Never
        };
        let db_path = dir.join("db");

        #[cfg(feature="glyphmatcher")]
        let font_db = db_path.is_dir().then(|| FontDb::new(db_path));
        dbg!(&dump);

        StandardCache {
            inner: SyncCache::new(),
            dir,
            fonts,
            dump,
            #[cfg(feature="glyphmatcher")]
            font_db,
            require_unique_unicode: false,
        }
    }
    pub fn require_unique_unicode(&mut self, r: bool) {
        self.require_unique_unicode = r;
    }
}

#[derive(Debug)]
enum Dump {
    Never,
    OnError,
    Always
}

pub struct GlyphData {
    encoding: vello_encoding::Encoding,
    offsets: Vec<Offset>
}
struct Offset {
    path_tag: usize,
    path_data: usize,
    n_path_segments: u32,
}
impl GlyphData {
    pub fn new() -> Self {
        GlyphData { encoding: Encoding::new(), offsets: vec![] }
    }
}
impl font::Encoder for GlyphData {
    type Pen<'a> = PathEncoder<'a>;
    type GlyphRef = u32;
    fn encode_shape<'f, O, E>(&mut self, f: impl for<'a> FnMut(&'a mut Self::Pen<'a>) -> Result<O, E> + 'f) -> Result<(O, Self::GlyphRef), E> {
        let mut p = self.encoding.encode_path(true);
        let o = f(&mut p)?;
        p.finish(true);
        self.offsets.push(Offset {
            path_tag: self.encoding.path_tags.len(),
            path_data: self.encoding.path_data.len(),
            n_path_segments: self.encoding.n_path_segments,
        });

        Ok((o, self.encoding.n_paths))
    }
}


pub fn load_font<E: Encoder + 'static>(encoder: &mut E, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve, cache: &StandardCache<E>) -> Result<Option<FontEntry<E>>>
    where FontRc<E>: Send
{
    let pdf_font = font_ref.clone();
    debug!("loading {:?}", pdf_font);
    
    let font: FontRc<E> = match pdf_font.embedded_data(resolve) {
        Some(Ok(data)) => {
            debug!("loading embedded font");
            let font = font::parse(&data, encoder).map_err(|e| {
                PdfError::Other { msg: format!("Font Error: {:?}", e) }
            });
            if matches!(cache.dump, Dump::Always) || (matches!(cache.dump, Dump::OnError) && font.is_err()) {
                let name = format!("font_{}", pdf_font.name.as_ref().map(|s| s.as_str()).unwrap_or("unnamed"));
                std::fs::write(&name, &data).unwrap();
                println!("font dumped in {}", name);
            }
            font?.into()
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
                    let val = cache.inner.get(file_name.clone(), || {
                        let data = match std::fs::read(cache.dir.join(file_name)) {
                            Ok(data) => data,
                            Err(e) => {
                                warn!("can't open {} for {:?} {:?}", file_name, pdf_font.name, e);
                                return None;
                            }
                        };
                        match font::parse(&data, encoder) {
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

    Ok(Some(FontEntry::build(font, pdf_font, 
        #[cfg(feature="glyphmatcher")] cache.font_db.as_ref(),
        resolve, cache.require_unique_unicode)?))
}
