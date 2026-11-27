//! Uploaded files: format by magic bytes only, stored by content hash under `data_dir/uploads/`.
//!
//! A picture is not always kept as it came. A phone's photo is stored upright, without its EXIF (where and when it
//! was taken), no larger than a screen shows, and compressed; a photo saved as a PNG becomes a JPEG. Small
//! pictures, pictures you can see through, GIFs and PDFs are kept byte for byte.

use crate::App;
use image::{DynamicImage, ImageDecoder, ImageEncoder};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::Path;

/// The long edge a stored picture may have: more than the widest size a page asks for, so nothing shown is enlarged.
pub const MAX_EDGE: u32 = 2560;
/// The long edge of the copy a model is shown: more pixels than it looks at only cost time and money.
pub const MODEL_EDGE: u32 = 1568;
/// JPEG quality of what is stored.
const QUALITY: u8 = 85;
/// A PNG smaller than this is a logo, an icon, a small graphic: never re-made.
const PNG_PHOTO_BYTES: usize = 100 * 1024;

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_BYTES_INLINE: usize = 5 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
/// Request body limit for routes that carry one upload: the file plus room for the multipart framing and form fields.
pub const UPLOAD_BODY_LIMIT: usize = MAX_BYTES + 1024 * 1024;

const ACCEPTED: &str = "JPEG、PNG、GIF、WebP、PDF";

/// (extension, content type) from the file header.
fn sniff(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else if b.starts_with(b"%PDF-") {
        Some(("pdf", "application/pdf"))
    } else {
        None
    }
}

fn content_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

pub fn check_pixels(width: u32, height: u32) -> Result<(), String> {
    let pixels = width as u64 * height as u64;
    if pixels > MAX_PIXELS {
        return Err(format!("图片太大：{width}×{height} 共 {pixels} 像素，最多 {MAX_PIXELS} 像素，请先缩小再上传。"));
    }
    Ok(())
}

struct Decoded {
    /// The picture, upright.
    img: DynamicImage,
    /// Its colour profile, carried over so colours do not shift.
    icc: Option<Vec<u8>>,
    /// It was sideways, or it carried EXIF: it cannot be kept as it came.
    changed: bool,
}

fn decode(bytes: &[u8]) -> Option<Decoded> {
    let mut decoder = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let exif = decoder.exif_metadata().ok().flatten().is_some();
    let icc = decoder.icc_profile().ok().flatten();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(Decoded { img, icc, changed: exif || orientation != image::metadata::Orientation::NoTransforms })
}

/// The picture with its long edge no longer than `edge`, and whether that made it smaller.
fn fit(img: DynamicImage, edge: u32) -> (DynamicImage, bool) {
    if img.width().max(img.height()) <= edge {
        return (img, false);
    }
    (img.resize(edge, edge, image::imageops::FilterType::CatmullRom), true)
}

fn see_through(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255)
}

fn encode(img: &DynamicImage, jpeg: bool, quality: u8, icc: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if jpeg {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        if let Some(profile) = icc {
            let _ = enc.set_icc_profile(profile);
        }
        DynamicImage::ImageRgb8(img.to_rgb8()).write_with_encoder(enc).ok()?;
    } else {
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        if let Some(profile) = icc {
            let _ = enc.set_icc_profile(profile);
        }
        img.write_with_encoder(enc).ok()?;
    }
    Some(out)
}

/// What is kept of an uploaded picture when that is not the upload itself: (bytes, extension).
// ponytail: a JPEG that only carries EXIF is compressed again to drop it; cut the EXIF segment out of the file
// instead if that second compression ever shows.
fn kept(bytes: &[u8], ext: &'static str) -> Option<(Vec<u8>, &'static str)> {
    if !matches!(ext, "jpg" | "png" | "webp") {
        return None;
    }
    let d = decode(bytes)?;
    let (img, shrunk) = fit(d.img, MAX_EDGE);
    let must = shrunk || d.changed;
    let clear = see_through(&img);
    let png = |img: &DynamicImage, icc| must.then(|| encode(img, false, QUALITY, icc)).flatten().map(|b| (b, "png"));
    match ext {
        // A photo saved as a PNG: a JPEG when that is well under half the size. A graphic does not shrink like that.
        "png" if !clear && bytes.len() > PNG_PHOTO_BYTES => {
            let jpeg = encode(&img, true, QUALITY, d.icc.clone())?;
            if jpeg.len() * 5 < bytes.len() * 2 { Some((jpeg, "jpg")) } else { png(&img, d.icc) }
        }
        "png" => png(&img, d.icc),
        // The encoder here only writes lossless WebP, which is no smaller: a WebP is re-made only when it must be.
        "webp" if !must => None,
        "webp" if clear => encode(&img, false, QUALITY, d.icc).map(|b| (b, "png")),
        _ => {
            let jpeg = encode(&img, true, QUALITY, d.icc)?;
            // Compressed again only when that saves a fifth: a photo is not worn down upload after upload.
            (must || jpeg.len() * 5 < bytes.len() * 4).then_some((jpeg, "jpg"))
        }
    }
}

/// The picture as a model should get it: (content type, bytes). Upright, its long edge at most `MODEL_EDGE`, without
/// EXIF. `None`: not a picture.
pub fn for_model(bytes: &[u8]) -> Option<(&'static str, Vec<u8>)> {
    let (ext, kind) = sniff(bytes).filter(|(ext, _)| *ext != "pdf")?;
    let as_it_is = || Some((kind, bytes.to_vec()));
    if ext == "gif" {
        return as_it_is();
    }
    let Some(d) = decode(bytes) else { return as_it_is() };
    let (img, shrunk) = fit(d.img, MODEL_EDGE);
    if !shrunk && !d.changed {
        return as_it_is();
    }
    let clear = see_through(&img);
    encode(&img, !clear, 80, None).map(|b| (if clear { "image/png" } else { "image/jpeg" }, b))
}

/// Validates and stores the bytes; returns `/uploads/<name>`. The claimed filename is only used in messages.
pub fn store_upload(app: &App, filename: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err(format!("“{filename}”是空文件：请上传有内容的文件。"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("“{filename}”太大：最大 {} MB。", MAX_BYTES >> 20));
    }
    let (ext, _) = sniff(bytes).ok_or_else(|| format!("“{filename}”的文件格式不支持：只接受 {ACCEPTED}（按文件内容判断，不看扩展名）。"))?;
    if ext != "pdf" {
        let (w, h) = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| format!("无法读取图片“{filename}”：{e}。"))?
            .into_dimensions()
            .map_err(|e| format!("无法读取图片“{filename}”的尺寸，文件可能已损坏：{e}。"))?;
        check_pixels(w, h).map_err(|e| format!("“{filename}”：{e}"))?;
    }
    let made = kept(bytes, ext);
    let (bytes, ext) = made.as_ref().map_or((bytes, ext), |(b, e)| (b.as_slice(), *e));
    let name = format!("{}.{ext}", &hex::encode(Sha256::digest(bytes))[..32]);
    let dir = app.data_dir.join("uploads");
    let io = |e: std::io::Error| format!("保存文件“{filename}”失败：{e}。");
    std::fs::create_dir_all(&dir).map_err(io)?;
    let dest = dir.join(&name);
    if !dest.exists() {
        // Write under a temporary name, then rename, so a half-written file is never served.
        // A random suffix keeps two simultaneous uploads of the same file from sharing a temporary file.
        let tmp = dir.join(format!(".{name}.{}.{:016x}.tmp", std::process::id(), rand::random::<u64>()));
        std::fs::write(&tmp, bytes).map_err(io)?;
        std::fs::rename(&tmp, &dest).map_err(io)?;
    }
    Ok(format!("/uploads/{name}"))
}

/// The content type of a stored file's name; only `<32 lowercase hex>.<ext>` is one.
fn known(name: &str) -> Option<&'static str> {
    let (stem, ext) = name.split_once('.')?;
    let ct = content_type(ext)?;
    (stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(ct)
}

/// `name` is the part after `/uploads/`; only `<32 lowercase hex>.<ext>` is accepted.
pub fn read(app: &App, name: &str) -> Option<(Vec<u8>, &'static str)> {
    let ct = known(name)?;
    Some((std::fs::read(app.data_dir.join("uploads").join(name)).ok()?, ct))
}

/// A stored picture's (width, height, bytes), read from the file's header.
pub fn info(app: &App, name: &str) -> Option<(u32, u32, u64)> {
    size_in(&app.data_dir.join("uploads"), name)
}

/// What an upload answers with: where the file is and, for a picture, what was kept of it (which for a photo is not
/// what was sent: upright, within the size limit, compressed).
pub fn stored(app: &App, path: String) -> serde_json::Value {
    match info(app, path.trim_start_matches("/uploads/")) {
        Some((width, height, bytes)) => serde_json::json!({ "path": path, "width": width, "height": height, "bytes": bytes }),
        None => serde_json::json!({ "path": path }),
    }
}

/// `info`, for a caller that has the uploads folder and not the app.
pub fn size_in(uploads: &Path, name: &str) -> Option<(u32, u32, u64)> {
    known(name).filter(|ct| ct.starts_with("image/"))?;
    let path = uploads.join(name);
    let (w, h) = image::ImageReader::open(&path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()?;
    Some((w, h, std::fs::metadata(&path).ok()?.len()))
}

/// Stored uploads as `(name, is image)`, newest first. A missing folder is an empty list.
pub fn list(app: &App) -> Vec<(String, bool)> {
    let Ok(dir) = std::fs::read_dir(app.data_dir.join("uploads")) else { return vec![] };
    let mut files: Vec<(std::time::SystemTime, String)> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.file_name().into_string().ok()?)))
        // Only names `read` would serve: this skips temporary files.
        .filter(|(_, n)| n.split_once('.').is_some_and(|(stem, ext)| stem.len() == 32 && content_type(ext).is_some()))
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    files.into_iter().map(|(_, n)| { let image = !n.ends_with(".pdf"); (n, image) }).collect()
}

/// Widths an image can be resized to (`/uploads/<name>?w=640`, or `{{ src|thumb(640) }}` in a template).
pub const WIDTHS: [u32; 5] = [384, 640, 828, 1200, 1920];

/// The allowed width that serves `w`: the smallest one at least as wide, else the largest.
pub fn snap_width(w: u32) -> u32 {
    WIDTHS.iter().copied().find(|&x| x >= w).unwrap_or(WIDTHS[WIDTHS.len() - 1])
}

/// An uploaded image no wider than `width`, made once and kept under `data_dir/thumbs/`. PNG stays PNG (logos and
/// cut-outs keep their edges and transparency); JPEG and WebP become JPEG. An image already narrow enough, a GIF
/// (it may move) or a PDF comes back as it is.
pub fn thumb(app: &App, name: &str, width: u32) -> Option<(Vec<u8>, &'static str)> {
    let (bytes, ct) = read(app, name)?;
    if !WIDTHS.contains(&width) || !matches!(ct, "image/jpeg" | "image/png" | "image/webp") {
        return Some((bytes, ct));
    }
    let png = ct == "image/png";
    let (out_ct, out_ext) = if png { ("image/png", "png") } else { ("image/jpeg", "jpg") };
    // The name is the file's hash, so a thumbnail never goes stale.
    let dir = app.data_dir.join("thumbs");
    let cached = dir.join(format!("{}-{width}.{out_ext}", name.split('.').next()?));
    if let Ok(b) = std::fs::read(&cached) {
        return Some((b, out_ct));
    }
    let made = (|| -> Result<Option<Vec<u8>>, image::ImageError> {
        use image::{DynamicImage, ImageDecoder, ImageReader};
        let mut decoder = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?.into_decoder()?;
        // Phone and camera files are often stored sideways.
        let orientation = decoder.orientation()?;
        let mut img = DynamicImage::from_decoder(decoder)?;
        if img.width() <= width && orientation == image::metadata::Orientation::NoTransforms {
            return Ok(None); // never enlarged
        }
        img.apply_orientation(orientation);
        if img.width() > width {
            let height = (u64::from(img.height()) * u64::from(width) / u64::from(img.width())).max(1) as u32;
            img = img.resize_exact(width, height, image::imageops::FilterType::CatmullRom);
        }
        let mut out = Vec::new();
        if png {
            img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82).encode_image(&img.to_rgb8())?;
        }
        Ok(Some(out))
    })();
    match made {
        Ok(Some(out)) => {
            // Written under a temporary name, then renamed: a half-written thumbnail is never served.
            let tmp = dir.join(format!(".{}.{:016x}.tmp", name, rand::random::<u64>()));
            if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&tmp, &out)).and_then(|_| std::fs::rename(&tmp, &cached)).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            Some((out, out_ct))
        }
        // Too small to shrink, or not decodable: the original.
        _ => Some((bytes, ct)),
    }
}
