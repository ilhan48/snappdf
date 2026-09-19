use genpdf::elements::Image as GenImage;
use genpdf::Scale;
use image::GenericImageView;
use std::collections::HashMap;
use std::sync::Mutex;

/// Görsel önbelleği: aynı URL'yi bir render içinde iki kez indirmemek için.
static CACHE: Mutex<Option<HashMap<String, image::DynamicImage>>> = Mutex::new(None);

/// Bu boyutun altındaki görseller (izleyici pikseli, ikon, ayraç) PDF'e girmez.
const MIN_PIXELS: u32 = 24;

/// Görselin "doğal" ekran yoğunluğu varsayımı (mutlak punto/mm ölçekleme için).
const SCREEN_DPI: f64 = 96.0;

// --- yükleme sayaçları (kullanıcıya özet rapor için) ---
use std::sync::atomic::{AtomicUsize, Ordering};
static LOADED: AtomicUsize = AtomicUsize::new(0);
static SKIPPED: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

/// Görsel yükleme özeti.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageStats {
    /// PDF'e gömülen görseller.
    pub loaded: usize,
    /// Dekoratif/çok küçük olduğu için atlananlar.
    pub skipped: usize,
    /// İndirilemeyen/çözümlenemeyenler.
    pub failed: usize,
}

/// Sayaçları okur ve sıfırlar.
pub fn take_stats() -> ImageStats {
    ImageStats {
        loaded: LOADED.swap(0, Ordering::Relaxed),
        skipped: SKIPPED.swap(0, Ordering::Relaxed),
        failed: FAILED.swap(0, Ordering::Relaxed),
    }
}

fn record(outcome: &ImageOutcome) {
    let counter = match outcome {
        ImageOutcome::Loaded(_) => &LOADED,
        ImageOutcome::Skipped => &SKIPPED,
        ImageOutcome::Failed => &FAILED,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Görsel yükleme sonucu.
pub enum ImageOutcome {
    /// PDF'e gömülmeye hazır görsel.
    Loaded(Box<GenImage>),
    /// Dekoratif/çok küçük — sessizce atlanır.
    Skipped,
    /// İndirilemedi/çözümlenemedi — istenirse not bırakılır.
    Failed,
}

/// URL'den görsel indirir, çözümler ve PDF'e sığacak şekilde ölçekler.
///
/// `max_width_mm` / `max_height_mm` kullanılabilir alan ölçüleridir. Görsel
/// büyütülmez (küçük olanlar doğal boyutunda kalır) ama asla alanı taşmaz.
pub fn load(url: &str, max_width_mm: f64, max_height_mm: f64) -> ImageOutcome {
    load_with(
        url,
        max_width_mm,
        max_height_mm,
        || network_fetch(url),
        decode,
    )
}

/// `load` ile aynı, ancak sonucu sayaçlara da işler.
fn load_with<F, D>(
    url: &str,
    max_width_mm: f64,
    max_height_mm: f64,
    fetch: F,
    decode_fn: D,
) -> ImageOutcome
where
    F: FnOnce() -> Option<(bytes::Bytes, &'static str)>,
    D: FnOnce(bytes::Bytes, &str) -> Option<image::DynamicImage>,
{
    let outcome = load_inner(url, max_width_mm, max_height_mm, fetch, decode_fn);
    record(&outcome);
    outcome
}

/// Test edilebilir çekirdek: `fetch` ve `decode` bağımlılıkları parametrik.
fn load_inner<F, D>(
    url: &str,
    max_width_mm: f64,
    max_height_mm: f64,
    fetch: F,
    decode_fn: D,
) -> ImageOutcome
where
    F: FnOnce() -> Option<(bytes::Bytes, &'static str)>,
    D: FnOnce(bytes::Bytes, &str) -> Option<image::DynamicImage>,
{
    // Yalnızca http(s) desteklenir (data:/ftp: dışlanır).
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ImageOutcome::Failed;
    }

    let img = match cached(url) {
        Some(img) => img,
        None => {
            let Some((bytes, format)) = fetch() else {
                return ImageOutcome::Failed;
            };
            let Some(img) = decode_fn(bytes, format) else {
                return ImageOutcome::Failed;
            };
            store(url, &img);
            img
        }
    };

    prepare(img, max_width_mm, max_height_mm)
}

/// Çözümlenmiş görseli PDF elemanına dönüştürür: alfa beyaza düzleştirilir,
/// dekoratif boyutlar elenir, alan ölçüsüne göre ölçeklenir.
fn prepare(img: image::DynamicImage, max_width_mm: f64, max_height_mm: f64) -> ImageOutcome {
    let (px_w, px_h) = img.dimensions();
    if px_w < MIN_PIXELS || px_h < MIN_PIXELS {
        return ImageOutcome::Skipped;
    }

    // genpdf alfa kanallı görselleri reddeder; beyaz zemin üzerine bindirilir.
    let rgb = flatten_alpha(img);

    let native_w = 25.4 * f64::from(px_w) / SCREEN_DPI;
    let native_h = 25.4 * f64::from(px_h) / SCREEN_DPI;
    let k = fit_scale(native_w, native_h, max_width_mm, max_height_mm);

    match GenImage::from_dynamic_image(rgb) {
        Ok(elem) => ImageOutcome::Loaded(Box::new(
            elem.with_dpi(SCREEN_DPI)
                .with_scale(Scale::new(k, k))
                .with_alignment(genpdf::Alignment::Center),
        )),
        Err(_) => ImageOutcome::Failed,
    }
}

/// Alfa kanalını beyaz zemin üzerine bindirir (RGBA/RGBA-palette PNG'ler).
/// Alfa yoksa görsel olduğu gibi döner.
fn flatten_alpha(img: image::DynamicImage) -> image::DynamicImage {
    if !img.color().has_alpha() {
        return img;
    }
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = image::RgbImage::new(w, h);
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u32::from(p[3]);
        let blend = |c: u8| (((u32::from(c) * a) + 255 * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])]));
    }
    image::DynamicImage::ImageRgb8(out)
}

/// Görseli `max_w` x `max_h` mm alanına sığdıran ölçek katsayısı.
/// Küçük görseller büyütülmez (kırıklaşmasın), yalnızca alanı taşanlar küçültülür.
pub fn fit_scale(width_mm: f64, height_mm: f64, max_w: f64, max_h: f64) -> f64 {
    let mut k = 1.0_f64;
    if max_w > 0.0 && width_mm > max_w {
        k = max_w / width_mm;
    }
    if max_h > 0.0 && height_mm * k > max_h {
        k = max_h / height_mm;
    }
    k.clamp(0.01, 1.0)
}

/// Gerçek ağ üzerinden getiren varsayılan fetch.
fn network_fetch(url: &str) -> Option<(bytes::Bytes, &'static str)> {
    // pdf.rs senkron bağlamda çağırır; ağ için kısa ömürlü blok istek yapılır.
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::try_current().ok()?;
        rt.block_on(async {
            let client = crate::fetch::build_client(std::time::Duration::from_secs(15)).ok()?;
            crate::fetch::fetch_image(&client, url).await
        })
    })
}

fn decode(bytes: bytes::Bytes, format: &str) -> Option<image::DynamicImage> {
    let cursor = std::io::Cursor::new(bytes.to_vec());
    match format {
        "png" => image::io::Reader::with_format(cursor, image::ImageFormat::Png)
            .decode()
            .ok(),
        "jpeg" => image::io::Reader::with_format(cursor, image::ImageFormat::Jpeg)
            .decode()
            .ok(),
        "webp" => decode_webp(&bytes),
        _ => None,
    }
}

/// WebP çözer (kayıplı VP8, kayıpsız VP8L ve alfa kanalı birlikte).
///
/// `image` 0.23 yalnızca kayıplı VP8'i çözebildiği için `image-webp`
/// kullanılır: pek çok site kayıpsız/alfalı WebP sunar. Animasyonlu WebP'lerde
/// ilk kare alınır (PDF'te tek kare gösterilebilir).
fn decode_webp(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut buffer = vec![0u8; decoder.output_buffer_size()?];
    decoder.read_image(&mut buffer).ok()?;
    if decoder.has_alpha() {
        image::RgbaImage::from_raw(width, height, buffer).map(image::DynamicImage::ImageRgba8)
    } else {
        image::RgbImage::from_raw(width, height, buffer).map(image::DynamicImage::ImageRgb8)
    }
}

fn cached(url: &str) -> Option<image::DynamicImage> {
    let guard = CACHE.lock().ok()?;
    guard.as_ref()?.get(url).cloned()
}

fn store(url: &str, img: &image::DynamicImage) {
    if let Ok(mut guard) = CACHE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(url.to_string(), img.clone());
    }
}

/// Testler için önbelleği boşaltır.
#[cfg(test)]
pub fn clear_cache() {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
}

/// Önbellek ve yükleme sayaçları globaldir; görsel yükleyen testler bu kilidi
/// alarak birbirini bekleterek çalışır.
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Görsel yükleyen testler için paylaşılan kilit.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> bytes::Bytes {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png kodlanmalı");
        bytes::Bytes::from(out)
    }

    fn png_rgb_bytes(w: u32, h: u32, rgb: [u8; 3]) -> bytes::Bytes {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png kodlanmalı");
        bytes::Bytes::from(out)
    }

    fn loaded(outcome: ImageOutcome) -> Option<GenImage> {
        match outcome {
            ImageOutcome::Loaded(i) => Some(*i),
            _ => None,
        }
    }

    #[test]
    fn rejects_non_http_schemes() {
        let _guard = test_lock();
        clear_cache();
        assert!(matches!(
            load_with("data:image/png;base64,AAA", 170.0, 200.0, || None, decode),
            ImageOutcome::Failed
        ));
        assert!(matches!(
            load_with("ftp://x/y.png", 170.0, 200.0, || None, decode),
            ImageOutcome::Failed
        ));
        assert!(matches!(
            load_with("", 170.0, 200.0, || None, decode),
            ImageOutcome::Failed
        ));
    }

    #[test]
    fn decode_png_works_and_rejects_garbage() {
        let ok = decode(png_bytes(4, 4, [255, 0, 0, 255]), "png");
        assert!(ok.is_some());
        assert!(decode(bytes::Bytes::from_static(b"not an image"), "png").is_none());
        assert!(decode(png_bytes(4, 4, [0, 0, 0, 255]), "webp").is_none());
        assert!(decode(bytes::Bytes::from_static(b"RIFF....WEBPxx"), "webp").is_none());
    }

    /// Kayıpsız WebP kodlayıcısıyla üretilmiş gerçek bir WebP dosyası.
    fn webp_bytes(w: u32, h: u32, rgba: [u8; 4]) -> bytes::Bytes {
        let pixels: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(&pixels, w, h, image_webp::ColorType::Rgba8)
            .expect("webp kodlanması");
        bytes::Bytes::from(out)
    }

    #[test]
    fn decode_lossless_webp_with_alpha() {
        // image 0.23 kayıpsız WebP'yi çözemez; image-webp çözer.
        let data = webp_bytes(8, 8, [255, 0, 0, 128]);
        let img = decode(data, "webp").expect("webp çözülmeli");
        assert_eq!(img.dimensions(), (8, 8));
        assert!(img.color().has_alpha());
    }

    #[test]
    fn webp_without_alpha_decodes_as_rgb() {
        let pixels: Vec<u8> = (0..16).flat_map(|_| [0u8, 128, 255]).collect();
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(&pixels, 4, 4, image_webp::ColorType::Rgb8)
            .unwrap();
        let img = decode(bytes::Bytes::from(out), "webp").expect("webp çözülmeli");
        assert_eq!(img.dimensions(), (4, 4));
        assert_eq!(img.to_rgb8().get_pixel(0, 0).0, [0, 128, 255]);
    }

    #[test]
    fn webp_images_go_through_the_full_pipeline() {
        let _guard = test_lock();
        clear_cache();
        // 200x100 WebP: hazırlama adımından geçip PDF elemanı olmalı.
        let data = webp_bytes(200, 100, [10, 20, 30, 255]);
        let outcome = load_with(
            "http://example.invalid/foto.webp",
            170.0,
            200.0,
            || Some((data.clone(), "webp")),
            decode,
        );
        assert!(loaded(outcome).is_some(), "webp PDF elemanına dönüşmeli");
    }

    #[test]
    fn alpha_is_flattened_over_white() {
        // Tam saydam piksel beyaza, yarı saydam kırmızı pembeleşir.
        let transparent = flatten_alpha(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])),
        ));
        assert_eq!(transparent.to_rgb8().get_pixel(0, 0).0, [255, 255, 255]);

        let half_red = flatten_alpha(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 128])),
        ));
        let px = half_red.to_rgb8().get_pixel(0, 0).0;
        assert_eq!(px[0], 255);
        assert_eq!(px[1], 127);
        assert_eq!(px[2], 127);
    }

    #[test]
    fn rgb_images_pass_through_untouched() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([10, 20, 30]),
        ));
        let out = flatten_alpha(img);
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.to_rgb8().get_pixel(0, 0).0, [10, 20, 30]);
    }

    #[test]
    fn alpha_images_are_accepted_by_genpdf() {
        // genpdf alfa kanallı görselleri reddeder; düzleştirme bunu çözer.
        let _guard = test_lock();
        clear_cache();
        let url = "https://mock/alpha.png";
        let out = load_with(
            url,
            170.0,
            200.0,
            || Some((png_bytes(60, 40, [255, 0, 0, 128]), "png")),
            decode,
        );
        assert!(
            loaded(out).is_some(),
            "RGBA PNG PDF elemanına dönüşebilmeli"
        );
        clear_cache();
    }

    #[test]
    fn tiny_decorative_images_are_skipped_silently() {
        let _guard = test_lock();
        clear_cache();
        let out = load_with(
            "https://mock/pixel.png",
            170.0,
            200.0,
            || Some((png_rgb_bytes(1, 1, [0, 0, 0]), "png")),
            decode,
        );
        assert!(matches!(out, ImageOutcome::Skipped));
        clear_cache();
    }

    #[test]
    fn failed_fetch_and_decode_report_failed() {
        let _guard = test_lock();
        clear_cache();
        assert!(matches!(
            load_with("https://mock/broken.png", 170.0, 200.0, || None, decode),
            ImageOutcome::Failed
        ));
        clear_cache();
        assert!(matches!(
            load_with(
                "https://mock/garbage.png",
                170.0,
                200.0,
                || Some((bytes::Bytes::from_static(b"junk"), "png")),
                decode,
            ),
            ImageOutcome::Failed
        ));
        clear_cache();
    }

    #[test]
    fn fit_scale_never_upscales_and_shrinks_to_fit() {
        // Küçük görsel büyütülmez.
        assert_eq!(fit_scale(50.0, 30.0, 170.0, 200.0), 1.0);
        // Geniş görsel sütun genişliğine küçültülür.
        let k = fit_scale(400.0, 200.0, 170.0, 200.0);
        assert!((k - 0.425).abs() < 1e-9);
        // Çok uzun görsel yükseklik sınırına göre küçültülür.
        let k = fit_scale(100.0, 1000.0, 170.0, 200.0);
        assert!((k - 0.2).abs() < 1e-9);
        // Ölçek asla sıfıra inmez.
        assert!(fit_scale(100_000.0, 100_000.0, 170.0, 200.0) >= 0.01);
    }

    #[test]
    fn oversized_image_is_scaled_down_in_pdf_element() {
        let _guard = test_lock();
        clear_cache();
        // 1200 px genişlik, 96 DPI'da ~317 mm: sütuna sığması için küçültülmeli.
        let out = load_with(
            "https://mock/wide.png",
            170.0,
            200.0,
            || Some((png_rgb_bytes(1200, 400, [1, 2, 3]), "png")),
            decode,
        );
        assert!(loaded(out).is_some());
        clear_cache();
    }

    #[test]
    fn network_fetch_is_not_repeated_for_same_url() {
        let _guard = test_lock();
        clear_cache();
        let url = "https://mock/cached.png";
        let mut fetch_count = 0usize;

        let first = load_with(
            url,
            170.0,
            200.0,
            || {
                fetch_count += 1;
                Some((png_rgb_bytes(50, 50, [9, 9, 9]), "png"))
            },
            decode,
        );
        assert!(loaded(first).is_some());
        let second = load_with(
            url,
            170.0,
            200.0,
            || {
                fetch_count += 1;
                Some((png_rgb_bytes(50, 50, [9, 9, 9]), "png"))
            },
            decode,
        );
        assert!(loaded(second).is_some());
        assert_eq!(fetch_count, 1, "önbellek isabeti ağa gitmemeli");
        clear_cache();
    }

    #[test]
    fn stats_count_each_outcome_once() {
        let _guard = test_lock();
        clear_cache();
        let _ = take_stats();
        let _ = load_with(
            "https://mock/stats-ok.png",
            170.0,
            200.0,
            || Some((png_rgb_bytes(50, 50, [0, 0, 0]), "png")),
            decode,
        );
        let _ = load_with(
            "https://mock/stats-small.png",
            170.0,
            200.0,
            || Some((png_rgb_bytes(2, 2, [0, 0, 0]), "png")),
            decode,
        );
        let _ = load_with("https://mock/stats-fail.png", 170.0, 200.0, || None, decode);
        let stats = take_stats();
        assert_eq!(
            stats,
            ImageStats {
                loaded: 1,
                skipped: 1,
                failed: 1
            }
        );
        // Sayaçlar okunduktan sonra sıfırlanır.
        assert_eq!(take_stats(), ImageStats::default());
        clear_cache();
    }

    #[test]
    fn load_blocking_fails_gracefully_outside_runtime() {
        // Runtime olmayan bağlamda panik değil, Failed dönmeli.
        let _guard = test_lock();
        clear_cache();
        assert!(matches!(
            load("https://örnek-yok.example/a.png", 170.0, 200.0),
            ImageOutcome::Failed | ImageOutcome::Skipped
        ));
        clear_cache();
    }
}
