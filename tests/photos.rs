//! Photos: an upload is kept upright, without where and when it was taken, no larger than a screen shows, and
//! compressed. The model is shown a small copy. A page gives every screen the size it needs.

mod common;

use base64::Engine;
use cms_harness::media;
use common::*;
use image::{ImageDecoder, ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};
use serde_json::json;
use std::io::Cursor;

/// A soft picture with grain, as a camera makes: PNG cannot shrink it much, JPEG can.
fn photo(w: u32, h: u32) -> RgbImage {
    let mut seed = 12345u32;
    RgbImage::from_fn(w, h, |x, y| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let v = 120.0 + 90.0 * ((x as f32 / 37.0).sin() * (y as f32 / 53.0).cos()) + (seed >> 28) as f32;
        Rgb([v as u8, (v * 0.8) as u8, (255.0 - v) as u8])
    })
}

/// Every pixel its own colour: nothing shrinks this much.
fn noise(w: u32, h: u32) -> RgbImage {
    let mut seed = 99u32;
    let mut next = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 24) as u8
    };
    RgbImage::from_fn(w, h, |_, _| Rgb([next(), next(), next()]))
}

/// EXIF that says one thing: the camera was held sideways (orientation 6, turn a quarter clockwise to show).
const SIDEWAYS: [u8; 26] = [b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];

fn jpeg(img: &RgbImage, quality: u8, exif: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    if let Some(e) = exif {
        enc.set_exif_metadata(e.to_vec()).unwrap();
    }
    enc.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8).unwrap();
    out
}

fn encoded(img: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn has(bytes: &[u8], what: &[u8]) -> bool {
    bytes.windows(what.len()).any(|w| w == what)
}

/// Stores `bytes`; returns the path and what was kept.
fn kept(app: &cms_harness::Shared, name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let path = media::store_upload(app, name, bytes).unwrap();
    let (stored, _) = media::read(app, path.trim_start_matches("/uploads/")).unwrap();
    (path, stored)
}

#[test]
fn a_phone_photo_is_kept_upright_within_the_limit_and_without_its_metadata() {
    let app = app();
    // As the sensor saw it: wide, its left half red. The phone was held sideways.
    let shot = RgbImage::from_fn(2800, 700, |x, _| if x < 1400 { Rgb([220, 30, 30]) } else { Rgb([30, 30, 220]) });
    let original = jpeg(&shot, 95, Some(&SIDEWAYS));
    let turned = image::ImageReader::new(Cursor::new(&original)).with_guessed_format().unwrap().into_decoder().unwrap().orientation().unwrap();
    assert_eq!(turned, image::metadata::Orientation::Rotate90, "the test's photo really says it is sideways");

    let (path, stored) = kept(&app, "IMG_0001.JPG", &original);
    assert!(path.ends_with(".jpg"), "{path}");
    assert!(has(&original, b"Exif") && !has(&stored, b"Exif"), "where and when it was taken stays with whoever took it");
    let img = image::load_from_memory(&stored).unwrap().to_rgb8();
    assert_eq!((img.width(), img.height()), (640, 2560), "upright, its long edge at the limit");
    let (top, bottom) = (img.get_pixel(320, 300), img.get_pixel(320, 2260));
    assert!(top[0] > 150 && top[2] < 100 && bottom[2] > 150 && bottom[0] < 100, "the sensor's left is the picture's top: {top:?} {bottom:?}");
    assert_eq!(media::info(&app, path.trim_start_matches("/uploads/")), Some((640, 2560, stored.len() as u64)));
    assert_eq!(media::store_upload(&app, "again.jpg", &original).unwrap(), path, "the same photo is the same file");
}

#[test]
fn a_photo_is_compressed_once() {
    let app = app();
    let fresh = jpeg(&photo(1600, 1200), 98, None);
    let (path, stored) = kept(&app, "a.jpg", &fresh);
    assert!(stored.len() * 10 < fresh.len() * 8, "at least a fifth smaller: {} -> {}", fresh.len(), stored.len());
    assert_eq!(media::info(&app, path.trim_start_matches("/uploads/")).map(|(w, h, _)| (w, h)), Some((1600, 1200)), "nothing to turn, nothing to shrink");
    let (again, twice) = kept(&app, "b.jpg", &stored);
    assert!(again == path && twice == stored, "what is already compressed is left as it is");
}

#[test]
fn a_photo_saved_as_png_becomes_a_jpeg_and_graphics_stay_png() {
    let app = app();
    let shot = encoded(&photo(900, 600).into(), image::ImageFormat::Png);
    let (path, stored) = kept(&app, "screenshot.png", &shot);
    assert!(path.ends_with(".jpg") && stored.len() * 5 < shot.len() * 2, "{path}: {} -> {}", shot.len(), stored.len());

    let cut = encoded(&photo_with_hole(), image::ImageFormat::Png);
    let (path, stored) = kept(&app, "cutout.png", &cut);
    assert!(path.ends_with(".png") && stored == cut, "a picture you can see through keeps its transparency, byte for byte");

    let sharp = encoded(&noise(400, 300).into(), image::ImageFormat::Png);
    let (path, stored) = kept(&app, "chart.png", &sharp);
    assert!(path.ends_with(".png") && stored == sharp, "a PNG that JPEG would not shrink much stays a PNG");

    let logo = encoded(&RgbImage::from_pixel(64, 64, Rgb([20, 20, 20])).into(), image::ImageFormat::Png);
    assert_eq!(kept(&app, "logo.png", &logo).1, logo, "small pictures are never touched");
}

fn photo_with_hole() -> image::DynamicImage {
    let p = photo(900, 600);
    RgbaImage::from_fn(900, 600, |x, y| {
        let c = p.get_pixel(x, y);
        Rgba([c[0], c[1], c[2], if x < 100 && y < 100 { 0 } else { 255 }])
    })
    .into()
}

#[test]
fn other_formats_are_handled_as_they_need() {
    let app = app();
    let wide = encoded(&RgbImage::from_fn(3000, 60, |x, _| Rgb([(x % 255) as u8, 80, 80])).into(), image::ImageFormat::WebP);
    let (path, stored) = kept(&app, "banner.webp", &wide);
    let img = image::load_from_memory(&stored).unwrap();
    assert!(path.ends_with(".jpg") && img.width() == 2560, "a WebP over the limit is shrunk, and kept as a JPEG: {path} {}", img.width());
    let small = encoded(&photo(300, 200).into(), image::ImageFormat::WebP);
    assert_eq!(kept(&app, "small.webp", &small).1, small, "a WebP within the limit is left alone");
    let gif = encoded(&photo(40, 40).into(), image::ImageFormat::Gif);
    let (path, stored) = kept(&app, "anim.gif", &gif);
    assert!(path.ends_with(".gif") && stored == gif, "a GIF may move: it is never re-made");
}

#[test]
fn the_model_is_shown_a_small_upright_copy() {
    let (kind, small) = media::for_model(&jpeg(&photo(3136, 400), 90, None)).unwrap();
    let img = image::load_from_memory(&small).unwrap();
    assert_eq!((kind, img.width(), img.height()), ("image/jpeg", 1568, 200), "more pixels than a model looks at only cost time");

    let shot = RgbImage::from_fn(400, 100, |x, _| if x < 200 { Rgb([220, 30, 30]) } else { Rgb([30, 30, 220]) });
    let (_, upright) = media::for_model(&jpeg(&shot, 90, Some(&SIDEWAYS))).unwrap();
    let img = image::load_from_memory(&upright).unwrap().to_rgb8();
    assert!((img.width(), img.height()) == (100, 400) && img.get_pixel(50, 40)[0] > 150, "a sideways photo is turned for the model too");

    let tiny = encoded(&RgbImage::new(4, 4).into(), image::ImageFormat::Png);
    assert_eq!(media::for_model(&tiny), Some(("image/png", tiny.clone())), "a small picture goes as it is");
    let see_through = media::for_model(&encoded(&photo_with_hole().resize(2000, 2000, image::imageops::FilterType::Nearest), image::ImageFormat::Png)).unwrap();
    assert_eq!(see_through.0, "image/png", "transparency survives the shrinking");
    assert!(media::for_model(b"%PDF-1.4 not a picture").is_none() && media::for_model(b"MZ").is_none());
}

#[test]
fn the_upload_tool_says_what_was_kept() {
    let app = app();
    let fresh = jpeg(&photo(1600, 1200), 98, None);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&fresh);
    let out = ok(&app, &editor(), "upload_media", json!({"filename": "a.jpg", "data_base64": b64}));
    let (path, stored) = kept(&app, "a.jpg", &fresh);
    assert_eq!(out, json!({"path": path, "width": 1600, "height": 1200, "bytes": stored.len()}));
    let pdf = ok(&app, &editor(), "upload_media", json!({"filename": "a.pdf", "data_base64": base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 hi")}));
    assert!(pdf["path"].as_str().unwrap().ends_with(".pdf") && pdf.get("width").is_none(), "{pdf}");
}

#[tokio::test]
async fn a_page_gives_every_screen_the_size_it_needs() {
    let app = app();
    let (big, _) = kept(&app, "a.jpg", &jpeg(&photo(1600, 1200), 90, None));
    let (small, _) = kept(&app, "b.png", &encoded(&RgbImage::new(300, 200).into(), image::ImageFormat::Png));
    ok(&app, &builder(), "write_site_file", json!({"path": "index.html", "note": "x", "content": format!(
        "<p id=a>{{{{ img('{big}', alt='A <b>', sizes='50vw', class='cover') }}}}</p><p id=b>{{{{ img('{big}', eager=true) }}}}</p>\
         <p id=c>{{{{ img('{small}', alt='小图') }}}}</p><p id=d>{{{{ img('/logo.svg', alt='logo') }}}}</p><p id=e>{{{{ img('') }}}}{{{{ img(nothing) }}}}</p>")}));
    publish_all(&app);
    let home = get_text(&app, "/").await;
    assert!(home.contains(&format!(
        "<p id=a><img src=\"{big}?w=828\" srcset=\"{big}?w=384 384w, {big}?w=640 640w, {big}?w=828 828w, {big}?w=1200 1200w, {big} 1600w\" sizes=\"50vw\" width=\"1600\" height=\"1200\" alt=\"A &lt;b&gt;\" class=\"cover\" loading=\"lazy\" decoding=\"async\"></p>"
    )), "every width below its own, then itself; its real size so the page does not jump: {home}");
    assert!(home.contains("sizes=\"100vw\" width=\"1600\" height=\"1200\" alt=\"\" fetchpriority=\"high\" decoding=\"async\"></p><p id=c>"), "the first picture on a page is not lazy: {home}");
    assert!(home.contains(&format!("<p id=c><img src=\"{small}\" width=\"300\" height=\"200\" alt=\"小图\" loading=\"lazy\" decoding=\"async\"></p>")), "nothing smaller to offer: {home}");
    assert!(home.contains("<p id=d><img src=\"/logo.svg\" alt=\"logo\" loading=\"lazy\" decoding=\"async\"></p>"), "not an upload: {home}");
    assert!(home.contains("<p id=e></p>"), "no picture, no tag: {home}");
}

#[test]
fn the_page_check_points_at_pictures_sent_whole() {
    let app = app();
    let (big, _) = kept(&app, "a.jpg", &jpeg(&photo(1600, 1200), 90, None));
    let page = |body: String| {
        ok(&app, &builder(), "write_site_file", json!({"path": "index.html", "note": "x",
            "content": format!("<!doctype html><html lang=zh><head><title>首页</title><meta name=viewport content=\"width=device-width\"></head><body><h1>首页</h1>{body}</body></html>")}));
        ok(&app, &builder(), "check_page", json!({"path": "/"}))["warnings"].to_string()
    };
    assert!(page(format!("<img src=\"{big}\" alt=\"\">")).contains("img("), "a whole photo for a thumbnail");
    assert!(!page(format!("<img src=\"{big}?w=640\" alt=\"\">")).contains("img("));
    assert!(!page(format!("{{{{ img('{big}') }}}}")).contains("img("));
}
