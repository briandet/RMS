//! Artwork lookup (`Slim::Web::Graphics::artworkRequest`) and resizing (`Slim::Utils::GDResizer`).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};
use rusqlite::OptionalExtension;

use crate::Library;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spec {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub mode: Option<char>,
    pub bgcolor: Option<String>,
    pub ext: Option<String>,
    /// width/height were present but not numeric ("X")
    pub raw: String,
}

/// Extract the resize spec from the trailing path element (e.g. `cover_300x300_o.jpg` → `300x300_o.jpg`).
pub fn extract_spec(basename: &str) -> String {
    let re = crate::re!(r"_?((?:[0-9X]+x[0-9X]+)?(?:_\w)?(?:_[\da-fA-F]+)?(?:\.\w+)?)$");
    re.captures(basename).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_default()
}

/// `Slim::Web::Graphics::parseSpec`
pub fn parse_spec(spec: &str) -> Spec {
    let spec = spec.split(',').next().unwrap_or("");
    let re = crate::re!(r"^(?:([0-9X]+)x([0-9X]+))?(?:_(\w))?(?:_([\da-fA-F]+))?(?:\.(\w+))?$");
    let mut s = Spec { raw: spec.to_string(), ..Default::default() };
    if let Some(c) = re.captures(spec) {
        s.width = c.get(1).and_then(|m| m.as_str().parse().ok());
        s.height = c.get(2).and_then(|m| m.as_str().parse().ok());
        s.mode = c.get(3).and_then(|m| m.as_str().chars().next());
        s.bgcolor = c.get(4).map(|m| m.as_str().to_string());
        s.ext = c.get(5).map(|m| m.as_str().to_ascii_lowercase());
    }
    s
}

fn fmt_of(data: &[u8]) -> &'static str {
    match crate::tags::sniff_image_type(data) {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

fn ct_of(fmt: &str) -> String {
    match fmt {
        "jpg" => "image/jpeg".into(),
        f => format!("image/{}", f),
    }
}

fn parse_bg(bg: Option<&str>) -> Option<Rgba<u8>> {
    let bg = bg?;
    if bg.len() != 6 && bg.len() != 8 {
        return Some(Rgba([255, 255, 255, 255]));
    }
    let v = u32::from_str_radix(bg, 16).ok()?;
    if bg.len() == 6 {
        Some(Rgba([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]))
    } else {
        Some(Rgba([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]))
    }
}

fn fit_scaled(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    let (iw, ih) = img.dimensions();
    let scale = (w as f64 / iw as f64).min(h as f64 / ih as f64);
    let nw = ((iw as f64 * scale).round() as u32).max(1);
    let nh = ((ih as f64 * scale).round() as u32).max(1);
    img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
}

/// keep_aspect resize into a w×h canvas padded with `bg` (transparent when None).
fn resize_pad(img: &DynamicImage, w: u32, h: u32, bg: Option<Rgba<u8>>) -> DynamicImage {
    let scaled = fit_scaled(img, w, h);
    let (sw, sh) = scaled.dimensions();
    if sw == w && sh == h {
        return scaled;
    }
    let mut canvas = RgbaImage::from_pixel(w, h, bg.unwrap_or(Rgba([0, 0, 0, 0])));
    let x = (w - sw) / 2;
    let y = (h - sh) / 2;
    image::imageops::overlay(&mut canvas, &scaled.to_rgba8(), x as i64, y as i64);
    DynamicImage::ImageRgba8(canvas)
}

fn encode(img: &DynamicImage, fmt: &str, bg: Option<Rgba<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if fmt == "png" {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).ok()?;
    } else {
        // flatten alpha
        let rgba = img.to_rgba8();
        let bg = bg.unwrap_or(Rgba([255, 255, 255, 255]));
        let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
        for (x, y, p) in rgba.enumerate_pixels() {
            let a = p[3] as u32;
            let blend = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
            rgb.put_pixel(x, y, image::Rgb([blend(p[0], bg[0]), blend(p[1], bg[1]), blend(p[2], bg[2])]));
        }
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        enc.encode_image(&DynamicImage::ImageRgb8(rgb)).ok()?;
    }
    Some(out)
}

/// `GDResizer->resize` semantics. `spec` may be a full trailing element (`image_300x300_o.png`)
/// or a bare spec (`300x300_o`).
pub fn resize_image(src: &[u8], spec: &str) -> Option<(Vec<u8>, String)> {
    let spec_s = extract_spec(spec);
    let s = parse_spec(&spec_s);
    let in_fmt = fmt_of(src);
    let explicit = s.ext.as_deref().map(|e| match e {
        "jpeg" | "jpg" => "jpg",
        "png" => "png",
        "gif" => "gif",
        _ => "jpg",
    });
    let wh_raw = crate::re!(r"^([0-9X]+)x([0-9X]+)").captures(&s.raw).map(|c| (c[1].to_string(), c[2].to_string()));
    let mut width = s.width.filter(|w| *w > 0);
    let mut height = s.height.filter(|h| *h > 0);
    if width.is_none() && height.is_none() {
        if explicit.is_none() || explicit == Some(in_fmt) {
            return Some((src.to_vec(), ct_of(in_fmt)));
        }
    }
    if let Some((w, h)) = &wh_raw {
        let bad = |v: &str| v != "X" && v.parse::<u32>().is_err();
        if bad(w) || bad(h) {
            return Some((src.to_vec(), ct_of(in_fmt)));
        }
    }
    let mode = s.mode.unwrap_or('m');
    let bg = parse_bg(s.bgcolor.as_deref());
    let img = image::load_from_memory(src).ok()?;
    let (iw, ih) = img.dimensions();
    let mut format = match explicit {
        Some("png") => "png",
        Some(_) => "jpg",
        None => {
            if in_fmt == "jpg" {
                "jpg"
            } else {
                "png"
            }
        }
    };
    if width.is_none() && height.is_none() {
        width = Some(iw);
        height = Some(ih);
    }
    let out_img = match mode {
        'm' | 'p' => {
            if explicit.is_none() && format != "png" {
                if let (Some(w), Some(h)) = (width, height) {
                    if (ih as f64 / iw as f64) != (h as f64 / w as f64) {
                        format = "png";
                    }
                }
            }
            let (mut w, mut h) = (width, height);
            if let (Some(ww), Some(hh)) = (w, h) {
                if ww > iw && hh > ih {
                    if ih as f64 / iw as f64 > 1.0 {
                        h = Some((hh as f64 * (iw as f64 / ww as f64)) as u32);
                        w = Some(iw);
                    } else {
                        w = Some((ww as f64 * (ih as f64 / hh as f64)) as u32);
                        h = Some(ih);
                    }
                }
            }
            match (w, h) {
                (Some(w), Some(h)) => resize_pad(&img, w.max(1), h.max(1), bg),
                (Some(w), None) => {
                    let h = ((ih as f64 * w as f64 / iw as f64).round() as u32).max(1);
                    img.resize_exact(w.max(1), h, image::imageops::FilterType::Lanczos3)
                }
                (None, Some(h)) => {
                    let w = ((iw as f64 * h as f64 / ih as f64).round() as u32).max(1);
                    img.resize_exact(w, h.max(1), image::imageops::FilterType::Lanczos3)
                }
                (None, None) => img.clone(),
            }
        }
        'F' => {
            let w = width.unwrap_or(iw);
            let h = height.unwrap_or(ih);
            if w >= iw && h >= ih {
                img.clone()
            } else {
                resize_pad(&img, w, h, bg)
            }
        }
        _ => {
            // 'o' and everything else: width only, preserve aspect
            let mut w = width;
            if let (Some(ww), Some(hh)) = (width, height) {
                if ww > iw && hh > ih {
                    if ih as f64 / iw as f64 > 1.0 {
                        w = Some((ww as f64 * (ih as f64 / hh as f64)) as u32);
                    } else {
                        w = Some(iw);
                    }
                }
            }
            match (w, height) {
                (Some(w), _) => {
                    let h = ((ih as f64 * w as f64 / iw as f64).round() as u32).max(1);
                    img.resize_exact(w.max(1), h, image::imageops::FilterType::Lanczos3)
                }
                (None, Some(h)) => {
                    let w = ((iw as f64 * h as f64 / ih as f64).round() as u32).max(1);
                    img.resize_exact(w, h.max(1), image::imageops::FilterType::Lanczos3)
                }
                _ => img.clone(),
            }
        }
    };
    let bytes = encode(&out_img, format, bg)?;
    Some((bytes, ct_of(format)))
}

fn cache_path(lib: &Library, id: &str, spec: &str) -> PathBuf {
    let safe: String = spec.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' }).collect();
    lib.cache_dir().join("artwork").join(id).join(if safe.is_empty() { "orig".into() } else { safe })
}

/// Source image bytes for a track's `cover` value.
fn source_image(track_url: &str, cover: &str) -> Option<Vec<u8>> {
    if cover.is_empty() || cover == "0" {
        return None;
    }
    let audio_path = crate::url::path_from_file_url(track_url);
    if cover.chars().all(|c| c.is_ascii_digit()) {
        let p = audio_path?;
        return crate::tags::read_embedded_cover(&p).map(|(b, _)| b);
    }
    if cover.starts_with("http") {
        return None;
    }
    let p = Path::new(cover);
    if p.is_file() {
        return std::fs::read(p).ok();
    }
    // the image vanished: try the folder again, then the embedded art
    let ap = audio_path?;
    let dir = ap.parent()?;
    if let Some(f) = crate::scanner::find_standalone_artwork(dir, "") {
        return std::fs::read(f).ok();
    }
    crate::tags::read_embedded_cover(&ap).map(|(b, _)| b)
}

/// Artwork for `/music/<id>/<spec>`: `id` is a coverid (8 hex chars) or a numeric track id.
pub fn artwork(lib: &Library, id: &str, spec_elem: &str) -> Option<(Vec<u8>, String)> {
    let c = lib.conn().ok()?;
    let lookup = |sql: &str, v: &str| -> Option<(String, String)> {
        c.query_row(sql, [v], |r| {
            let u: String = r.get(0)?;
            let cover = crate::songdata::get_text(r, 1)?.unwrap_or_default();
            Ok((u, cover))
        })
        .optional()
        .ok()
        .flatten()
        .filter(|(_, cv)| !cv.is_empty() && cv != "0")
    };
    let mut found = None;
    let is_coverid = id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if is_coverid {
        found = lookup("SELECT url, cover FROM tracks WHERE coverid = ? LIMIT 1", id);
    }
    if found.is_none() && id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
        found = lookup("SELECT url, cover FROM tracks WHERE id = ?", id);
    }
    let (track_url, cover) = found?;
    drop(c);
    let spec = extract_spec(spec_elem);
    let parsed = parse_spec(&spec);
    let cache_file = cache_path(lib, id, &spec);
    if parsed.width.is_some() || parsed.height.is_some() || parsed.ext.is_some() {
        if let Ok(b) = std::fs::read(&cache_file) {
            if !b.is_empty() {
                let ct = crate::tags::sniff_image_type(&b).to_string();
                return Some((b, ct));
            }
        }
    }
    let src = source_image(&track_url, &cover)?;
    if spec.is_empty() {
        let ct = crate::tags::sniff_image_type(&src).to_string();
        return Some((src, ct));
    }
    let (out, ct) = resize_image(&src, &spec)?;
    if let Some(parent) = cache_file.parent() {
        let _ = std::fs::create_dir_all(parent);
        let tmp = cache_file.with_extension("tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            let _ = std::fs::rename(&tmp, &cache_file);
        }
    }
    Some((out, ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parsing() {
        assert_eq!(extract_spec("cover"), "");
        assert_eq!(extract_spec("cover.jpg"), ".jpg");
        assert_eq!(extract_spec("cover_300x300_o"), "300x300_o");
        assert_eq!(extract_spec("thumb_100x100_p.png"), "100x100_p.png");
        assert_eq!(extract_spec("cover_150x150_m"), "150x150_m");
        assert_eq!(extract_spec("300x300_o"), "300x300_o");
        let s = parse_spec("100x100_p_ff0000.png");
        assert_eq!(s.width, Some(100));
        assert_eq!(s.mode, Some('p'));
        assert_eq!(s.bgcolor.as_deref(), Some("ff0000"));
        assert_eq!(s.ext.as_deref(), Some("png"));
        let s = parse_spec("Xx200_f");
        assert_eq!(s.width, None);
        assert_eq!(s.height, Some(200));
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([200, 10, 10])));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn resize_modes() {
        let src = png(400, 200);
        // m: padded to exactly 100x100, png because aspect differs
        let (b, ct) = resize_image(&src, "cover_100x100_m").unwrap();
        assert_eq!(ct, "image/png");
        let i = image::load_from_memory(&b).unwrap();
        assert_eq!(i.dimensions(), (100, 100));
        // o: width only
        let (b, _) = resize_image(&src, "cover_100x100_o").unwrap();
        let i = image::load_from_memory(&b).unwrap();
        assert_eq!(i.dimensions(), (100, 50));
        // explicit jpg
        let (b, ct) = resize_image(&src, "cover_50x50_p.jpg").unwrap();
        assert_eq!(ct, "image/jpeg");
        assert_eq!(image::load_from_memory(&b).unwrap().dimensions(), (50, 50));
        // no size: original
        let (b, ct) = resize_image(&src, "cover").unwrap();
        assert_eq!(ct, "image/png");
        assert_eq!(b, src);
        // F: no upscale
        let (b, _) = resize_image(&src, "cover_800x800_F").unwrap();
        assert_eq!(image::load_from_memory(&b).unwrap().dimensions(), (400, 200));
        // m never upscales: box shrinks to the source's larger dimension
        let (b, _) = resize_image(&src, "cover_800x800_m").unwrap();
        assert_eq!(image::load_from_memory(&b).unwrap().dimensions(), (200, 200));
    }
}
