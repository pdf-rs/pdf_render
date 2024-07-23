use image::{RgbaImage, ImageBuffer, Rgba};
use pdf::object::*;
use pdf::error::PdfError;
use pathfinder_color::ColorU;
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use crate::BlendMode;

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct ImageData<'a> {
    data: Cow<'a, [ColorU]>,
    width: u32,
    height: u32,
}
impl<'a> ImageData<'a> {
    pub fn new(data: impl Into<Cow<'a, [ColorU]>>, width: u32, height: u32) -> Option<Self> {
        let data = data.into();
        if width as usize * height as usize != data.len() {
            return None;
        }
        Some(ImageData { data, width, height })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn data(&self) -> &[ColorU] {
        &*self.data
    }
    pub fn into_data(self) -> Cow<'a, [ColorU]> {
        self.data
    }
    pub fn rgba_data(&self) -> &[u8] {
        let ptr: *const ColorU = self.data.as_ptr();
        let len = self.data.len();
        unsafe {
            std::slice::from_raw_parts(ptr.cast(), 4 * len)
        }
    }
    /// angle must be in range 0 .. 4
    pub fn rotate(&self, angle: u8) -> ImageData<'_> {
        match angle {
            0 => ImageData {
                data: Cow::Borrowed(&*self.data),
                width: self.width,
                height: self.height
            },
            1 => {
                let mut data = Vec::with_capacity(self.data.len());
                
                for y in 0 .. self.width as usize {
                    for x in (0 .. self.height as usize).rev() {
                        data.push(self.data[x * self.width as usize + y]);
                    }
                }
                
                ImageData::new(
                    data,
                    self.height,
                    self.width
                ).unwrap()
            }
            2 => {
                let data: Vec<ColorU> = self.data.iter().rev().cloned().collect();
                ImageData::new(
                    data,
                    self.width,
                    self.height
                ).unwrap()
            }
            3 => {
                let mut data = Vec::with_capacity(self.data.len());
                
                for y in (0 .. self.width as usize).rev() {
                    for x in 0 .. self.height as usize {
                        data.push(self.data[x * self.width as usize + y]);
                    }
                }
                
                ImageData::new(
                    data,
                    self.height,
                    self.width
                ).unwrap()
            }
            _ => panic!("invalid rotation")
        }
    }

    pub fn safe(&self, path: &Path) {
        let data = self.rgba_data();
        ImageBuffer::<Rgba<u8>, &[u8]>::from_raw(self.width, self.height, data).unwrap().save(path).unwrap()
    }
}

fn resize_alpha(data: &[u8], src_width: u32, src_height: u32, dest_width: u32, dest_height: u32) -> Option<Vec<u8>> {
    use image::{ImageBuffer, imageops::{resize, FilterType}, Luma};

    let src: ImageBuffer<Luma<u8>, &[u8]> = ImageBuffer::from_raw(src_width, src_height, data)?;
    let dest = resize(&src, dest_width, dest_height, FilterType::CatmullRom);

    Some(dest.into_raw())
}

pub fn load_image(image: &ImageXObject, resources: &Resources, resolve: &impl Resolve, mode: BlendMode) -> Result<ImageData<'static>, PdfError> {
    let raw_data = image.image_data(resolve)?;

    let pixel_count = image.width as usize * image.height as usize;

    if raw_data.len() % pixel_count != 0 {
        warn!("invalid data length {} bytes for {} pixels", raw_data.len(), pixel_count);
        info!("image: {:?}", image.inner.info.info);
        info!("filters: {:?}", image.inner.filters);
    }

    enum Data<'a> {
        Arc(Arc<[u8]>),
        Vec(Vec<u8>),
        Slice(&'a [u8])
    }
    impl<'a> std::ops::Deref for Data<'a> {
        type Target = [u8];
        fn deref(&self) -> &[u8] {
            match self {
                Data::Arc(ref d) => &**d,
                Data::Vec(ref d) => &*d,
                Data::Slice(s) => s
            }
        }
    }
    impl<'a> From<Vec<u8>> for Data<'a> {
        fn from(v: Vec<u8>) -> Self {
            Data::Vec(v)
        }
    }

    let mask = t!(image.smask.map(|r| resolve.get(r)).transpose());
    let alpha = match mask {
        Some(ref mask) => {
            let data = Data::Arc(t!((**mask).data(resolve)));
            let mask_width = mask.width as usize;
            let mask_height = mask.height as usize;
            let bits_per_component = mask.bits_per_component.ok_or_else(|| PdfError::Other { msg: format!("no bits per component")})?;
            let bits = mask_width * mask_height * bits_per_component as usize;
            pdf_assert_eq!(data.len(), (bits + 7) / 8);

            let mut alpha: Data = match bits_per_component {
                1 => data.iter().flat_map(|&b| (0..8).map(move |i| ex(b >> i, 1))).collect::<Vec<u8>>().into(),
                2 => data.iter().flat_map(|&b| (0..4).map(move |i| ex(b >> 2*i, 2))).collect::<Vec<u8>>().into(),
                4 => data.iter().flat_map(|&b| (0..2).map(move |i| ex(b >> 4*i, 4))).collect::<Vec<u8>>().into(),
                8 => data,
                12 => data.chunks_exact(3).flat_map(|c| [c[0], c[1] << 4 | c[2] >> 4]).collect::<Vec<u8>>().into(),
                16 => data.chunks_exact(2).map(|c| c[0]).collect::<Vec<u8>>().into(),
                n => return Err(PdfError::Other { msg: format!("invalid bits per component {}", n)})
            };
            if mask.width != image.width || mask.height != image.height {
                alpha = resize_alpha(&*alpha, mask.width, mask.height, image.width, image.height).unwrap().into();
            }
            alpha
        }
        None => Data::Slice(&[][..])
    };
    #[inline]
    fn ex(b: u8, bits: u8) -> u8 {
        b & ((1 << bits) - 1)
    }
    
    fn resolve_cs<'a>(cs: &'a ColorSpace, resources: &'a Resources) -> Option<&'a ColorSpace> {
        match cs {
            ColorSpace::Icc(icc) => {
                match icc.info.alternate {
                    Some(ref b) => Some(&**b),
                    None => match icc.info.components {
                        1 => Some(&ColorSpace::DeviceGray),
                        3 => Some(&ColorSpace::DeviceRGB),
                        4 => Some(&ColorSpace::DeviceCMYK),
                        _ => None
                    }
                }
            }
            ColorSpace::Named(ref name) => resources.color_spaces.get(name),
            _ => Some(cs),
        }
    }

    let cs = image.color_space.as_ref().and_then(|cs| resolve_cs(cs, &resources));
    let alpha = alpha.iter().cloned().chain(std::iter::repeat(255));
    let data_ratio = (raw_data.len() * 8) / pixel_count;
    // dbg!(data_ratio);

    debug!("CS: {cs:?}");

    let data = match data_ratio {
        1 | 2 | 4 | 8 => {
            let pixel_data: Cow<[u8]> = match data_ratio {
                1 => raw_data.iter().flat_map(|&b| (0..8).map(move |i| ex(b >> i, 1))).take(pixel_count).collect::<Vec<u8>>().into(),
                2 => raw_data.iter().flat_map(|&b| (0..4).map(move |i| ex(b >> 2*i, 2))).take(pixel_count).collect::<Vec<u8>>().into(),
                4 => raw_data.iter().flat_map(|&b| (0..2).map(move |i| ex(b >> 4*i, 4))).take(pixel_count).collect::<Vec<u8>>().into(),
                8 => Cow::Borrowed(&raw_data[..pixel_count]),
                n => return Err(PdfError::Other { msg: format!("invalid bits per component {}", n)})
            };
            let pixel_data: &[u8] = &*pixel_data;
            // dbg!(&cs);
            match cs {
                Some(&ColorSpace::DeviceGray) => {
                    pdf_assert_eq!(pixel_data.len(), pixel_count);
                    pixel_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
                }
                Some(&ColorSpace::Indexed(ref base, hival, ref lookup)) => {
                    match resolve_cs(&**base, resources) {
                        Some(ColorSpace::DeviceRGB) => {
                            let mut data = Vec::with_capacity(pixel_data.len());
                            for (&b, a) in pixel_data.iter().zip(alpha) {
                                let off = b as usize * 3;
                                let c = lookup.get(off .. off + 3).ok_or(PdfError::Bounds { index: off, len: lookup.len() })?;
                                data.push(rgb2rgba(c, a, mode));
                            }
                            data
                        }
                        Some(ColorSpace::DeviceCMYK) => {
                            debug!("indexed CMYK {}", lookup.len());
                            let mut data = Vec::with_capacity(pixel_data.len());
                            for (&b, a) in pixel_data.iter().zip(alpha) {
                                let off = b as usize * 4;
                                let c = lookup.get(off .. off + 4).ok_or(PdfError::Bounds { index: off, len: lookup.len() })?;
                                data.push(cmyk2color(c.try_into().unwrap(), a, BlendMode::Darken));
                            }
                            data
                        }
                        _ => unimplemented!("base cs={:?}", base),
                    }
                }
                Some(&ColorSpace::Separation(_, ref alt, ref func)) => {
                    let mut lut = [[0u8; 3]; 256];

                    match resolve_cs(alt, resources) {
                        Some(ColorSpace::DeviceRGB) => {
                            for (i, rgb) in lut.iter_mut().enumerate() {
                                let mut c = [0.; 3];
                                func.apply(&[i as f32 / 255.], &mut c)?;
                                let [r, g, b] = c;
                                *rgb = rgb2rgb(r, g, b, mode);
                            }
                        }
                        Some(ColorSpace::DeviceCMYK) => {
                            for (i, rgb) in lut.iter_mut().enumerate() {
                                let mut c = [0.; 4];
                                func.apply(&[i as f32 / 255.], &mut c)?;
                                let [c, m, y, k] = c;
                                *rgb = cmyk2rgb([(c * 255.) as u8, (m * 255.) as u8, (y * 255.) as u8, (k * 255.) as u8], mode);
                            }
                        }
                        _ => unimplemented!("alt cs={:?}", alt),
                    }
                    pixel_data.iter().zip(alpha).map(|(&b, a)| {
                        let [r, g, b] = lut[b as usize];
                        ColorU { r, g, b, a }
                    }).collect()
                }
                None => {
                    info!("image has data/pixel ratio of 1, but no colorspace");
                    pdf_assert_eq!(pixel_data.len(), pixel_count);
                    pixel_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
                }
                _ => unimplemented!("cs={:?}", cs),
            }
        }
        24 => {
            if !matches!(cs, Some(ColorSpace::DeviceRGB)) {
                info!("image has data/pixel ratio of 3, but colorspace is {:?}", cs);
            }
            raw_data[..pixel_count * 3].chunks_exact(3).zip(alpha).map(|(c, a)| rgb2rgba(c, a, mode)).collect()
        }
        32 => {
            if !matches!(cs, Some(ColorSpace::DeviceCMYK)) {
                info!("image has data/pixel ratio of 4, but colorspace is {:?}", cs);
            }
            cmyk2color_arr(&raw_data[..pixel_count * 4], alpha, mode)
        }
        _ => unimplemented!("data/pixel ratio {}", data_ratio),
    };

    let data_len = data.len();
    match ImageData::new(data, image.width as u32, image.height as u32) {
        Some(data) => Ok(data),
        None => {
            warn!("image width: {}", image.width);
            warn!("image height: {}", image.height);
            warn!("data.len(): {}", data_len);
            warn!("data_ratio: {data_ratio}");
            Err(PdfError::Other { msg: "size mismatch".into() })
        }
    }
}

fn rgb2rgba(c: &[u8], a: u8, mode: BlendMode) -> ColorU {
    match mode {
        BlendMode::Overlay => {
            ColorU { r: c[0], g: c[1], b: c[2], a }
        }
        BlendMode::Darken => {
            ColorU { r: 255 - c[0], g: 255 - c[1], b: 255 - c[2], a }
        }
    }
    
}
fn rgb2rgb(r: f32, g: f32, b: f32, mode: BlendMode) -> [u8; 3] {
    match mode {
        BlendMode::Overlay => {
            [ (255. * r) as u8, (255. * g) as u8, (255. * b) as u8 ]
        }
        BlendMode::Darken => {
            [ 255 - (255. * r) as u8, 255 - (255. * g) as u8, 255 - (255. * b) as u8 ]
        }
    }
    
}
/*
red = 1.0 – min ( 1.0, cyan + black )
green = 1.0 – min ( 1.0, magenta + black )
blue = 1.0 – min ( 1.0, yellow + black )
*/

#[inline]
fn cmyk2rgb([c, m, y, k]: [u8; 4], mode: BlendMode) -> [u8; 3] {
    match mode {
        BlendMode::Darken => {
            let r = 255 - c.saturating_add(k);
            let g = 255 - m.saturating_add(k);
            let b = 255 - y.saturating_add(k);
            [r, g, b]
        }
        BlendMode::Overlay => {
            let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
            let r = 255 - c.saturating_add(k);
            let g = 255 - m.saturating_add(k);
            let b = 255 - y.saturating_add(k);
            [r, g, b]
        }
    }
}

#[inline]
fn cmyk2color(cmyk: [u8; 4], a: u8, mode: BlendMode) -> ColorU {
    let [r, g, b] = cmyk2rgb(cmyk, mode);
    ColorU::new(r, g, b, a)
}

fn cmyk2color_arr(data: &[u8], alpha: impl Iterator<Item=u8>, mode: BlendMode) -> Vec<ColorU> {
    data.chunks_exact(4).zip(alpha).map(|(c, a)| {
        let mut buf = [0; 4];
        buf.copy_from_slice(c);
        cmyk2color(buf, a, mode)
    }).collect()
}