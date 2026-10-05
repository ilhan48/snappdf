//! Görsel hattı: indirme, çözümleme, ölçekleme.
//!
//! Sorumluluklar bilinçli olarak tek yerde toplandı:
//!
//! 1. **İndirme** (`ImageLoader::prefetch`) — tüm görseller render'dan *önce*,
//!    sınırlı eşzamanlılıkla ve **tek** HTTP istemcisiyle indirilir. Daha önce
//!    her görsel kendi istemcisini senkron (`block_in_place`) kuruyordu: yavaş
//!    idi ve üstelik `Referer` göndermediği için sıcak korumalı CDN'ler
//!    (GitHub camo, Cloudinary, imgur) görselleri reddediyordu.
//! 2. **Çözümleme** — PNG/JPEG/WebP/GIF + **SVG** (vektörel görseller için
//!    gömülü fontlarla rasterizasyon). SVG desteği olmadan dokümantasyon
//!    sitelerinin (Rust kitabı, MDN, Read the Docs) görsellerinin tamamı
//!    kayboluyordu.
//! 3. **Hazırlama** (`prepare`) — alfa düzleştirme, dekoratif küçük görsellerin
//!    elenmesi, sütuna sığdırma.
//!
//! Render aşaması (`ImageLoader::element`) yalnızca önbelleğe bakar; ağ erişimi
//! yoktur, bu yüzden PDF üretimi bloklanmaz ve testler çevrimdışıdır.

use genpdf::elements::Image as GenImage;
use genpdf::Scale;
use image::GenericImageView;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Aynı anda indirilecek en fazla görsel.
const PREFETCH_CONCURRENCY: usize = 6;

/// Görsel isteği zaman aşımı.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Bu boyutun altındaki görseller (izleyici pikseli, ikon, ayraç) PDF'e girmez.
const MIN_PIXELS: u32 = 24;

/// Görselin "doğal" ekran yoğunluğu varsayımı (mutlak punto/mm ölçekleme için).
const SCREEN_DPI: f64 = 96.0;

/// SVG raster hedefi: doğal boyutun bu katı.
///
/// SVG'nin doğal çözünürlüğü CSS pikseli = `SCREEN_DPI` (96 dpi) kabul edilir;
/// doğal ölçekte rasterize edilen bir şema PDF'te bulanık görünürdü. 2.1 kat
/// ~200 dpi'ye denk gelir, yani yazıcıda da net çıkar.
const SVG_RASTER_SCALE: f64 = 2.1;

/// Rasterda izin verilen en uzun kenar (piksel). Aşırı büyük `viewBox`'lu
/// çizimler kâğıdı kaplamasın diye sınırlanır.
const SVG_MAX_EDGE_PX: f64 = 2400.0;

/// SVG rasterında izin verilen en fazla piksel sayısı (bellek koruması: hatalı
/// `viewBox` ile 100k x 100k'lık bir çizim PDF'i kilitlemesin).
const SVG_MAX_PIXELS: f64 = 16_000_000.0;

/// Tanınan raster biçimleri ve SVG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// PNG
    Png,
    /// JPEG
    Jpeg,
    /// WebP (kayıplı VP8, kayıpsız VP8L, alfa)
    Webp,
    /// GIF (ilk kare)
    Gif,
    /// SVG (vektörel; rasterize edilir)
    Svg,
}

impl Format {
    /// İçerik-tipi başlığından biçim çözer.
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        let ct = content_type.to_ascii_lowercase();
        if ct.contains("png") {
            Some(Self::Png)
        } else if ct.contains("jpeg") || ct.contains("jpg") {
            Some(Self::Jpeg)
        } else if ct.contains("webp") {
            Some(Self::Webp)
        } else if ct.contains("gif") {
            Some(Self::Gif)
        } else if ct.contains("svg") {
            Some(Self::Svg)
        } else {
            None
        }
    }

    /// Sihirli baytlardan biçim çözer (içerik-tipi `application/octet-stream`
    /// dönen sunucular için).
    pub fn sniff(data: &[u8]) -> Option<Self> {
        if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            return Some(Self::Png);
        }
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }
        if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        // SVG kök etiketi; XML prologu ya da yorum olabilir.
        let head = &data[..data.len().min(1024)];
        let text = String::from_utf8_lossy(head);
        let text = text.trim_start_matches('\u{feff}').trim_start();
        if text.starts_with('<') && text.contains("<svg") {
            return Some(Self::Svg);
        }
        None
    }

    /// `image` çözümleyicisine verilecek biçim (SVG hariç).
    fn raster(self) -> Option<image::ImageFormat> {
        match self {
            Self::Png => Some(image::ImageFormat::Png),
            Self::Jpeg => Some(image::ImageFormat::Jpeg),
            Self::Webp => None, // ayrı çözücü
            Self::Gif => Some(image::ImageFormat::Gif),
            Self::Svg => None,
        }
    }
}

/// Görsel URL'sinden kısa, okunabilir bir etiket (`host/a.png`).
///
/// PDF'e gömülemeyen görselin yerine basılan notta kullanılır: tam URL
/// sayfayı bozar, host + dosya adı teşhis için yeterlidir.
pub fn host_label(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return url.chars().take(48).collect();
    };
    let file = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or("görsel");
    format!("{}/{file}", parsed.host_str().unwrap_or("?"))
}

/// Görselin neden PDF'e giremediği.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Sunucuya ulaşılamadı / bozuk yanıt.
    Network,
    /// Baytlar geldi ama çözülemedi.
    Decode,
    /// Biçim tanınmıyor ya da rasterize edilemiyor (AVIF, HEIC ...).
    Unsupported,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => write!(f, "indirilemedi"),
            Self::Decode => write!(f, "çözümlenemedi"),
            Self::Unsupported => write!(f, "desteklenmeyen biçim"),
        }
    }
}

/// Görsel yükleme sonucu.
pub enum ImageOutcome {
    /// PDF'e gömülmeye hazır görsel.
    Loaded(Box<GenImage>),
    /// Dekoratif/çok küçük — sessizce atlanır.
    Skipped,
    /// PDF'e gömülemedi (nedeni `Display` ile okunur).
    Failed(Failure),
}

/// Önbellekteki çözülmüş görsel.
#[derive(Debug, Clone)]
enum Cached {
    /// Raster görsel (SVG rasterize edilerek de buraya girer).
    Image(Decoded),
    /// Başarıyla indirildi ama dekoratif olduğu için elendi.
    Skipped,
    /// İndirilemedi / çözülemedi.
    Failed(Failure),
}

/// Çözülmüş görsel ve **doğal boyut çözünürlüğü**.
///
/// SVG'ler 2.1 kat büyütülerek rasterize edilir; piksel sayısını doğal ölçekte
/// (96 dpi) yorumlarsak görsel PDF'te gereksiz büyük basılır. Bu yüzden
/// raster ölçeği `dpi` alanına yazılır: `natürel_mm = 25.4 * piksel / dpi`.
#[derive(Debug, Clone)]
struct Decoded {
    /// Pikseller.
    img: Arc<image::DynamicImage>,
    /// Bu piksel verisinin doğal çözünürlüğü (piksel/inç).
    dpi: f64,
}

impl Decoded {
    fn raster(img: image::DynamicImage) -> Self {
        Self {
            img: Arc::new(img),
            dpi: SCREEN_DPI,
        }
    }

    /// Doğal ölçüler (mm).
    fn natural_mm(&self) -> (f64, f64) {
        let (w, h) = image::GenericImageView::dimensions(self.img.as_ref());
        (
            25.4 * f64::from(w) / self.dpi,
            25.4 * f64::from(h) / self.dpi,
        )
    }
}

/// Görsel yükleme özeti.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageStats {
    /// PDF'e gömülen görseller.
    pub loaded: usize,
    /// Dekoratif/çok küçük olduğu için atlananlar.
    pub skipped: usize,
    /// İndirilemeyen/çözümlenemeyenler.
    pub failed: usize,
    /// SVG'den rasterize edilen görseller.
    pub rasterized: usize,
}

impl std::fmt::Display for ImageStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} gömüldü, {} dekoratif atlandı, {} yüklenemedi",
            self.loaded, self.skipped, self.failed
        )?;
        if self.rasterized > 0 {
            write!(f, " ({} SVG rasterize edildi)", self.rasterized)?;
        }
        Ok(())
    }
}

/// Görsel kaynağı: paylaşılan HTTP istemcisi + çözülmüş görsel önbelleği.
///
/// Render sırasında yalnızca `element` çağrılır (ağ erişimi yok); indirme
/// `prefetch` ile önceden yapılır.
#[derive(Debug)]
pub struct ImageLoader {
    client: reqwest::Client,
    /// Kaynak sayfanın adresi: sıcak korumalı CDN'ler `Referer` ister.
    referer: String,
    cache: Mutex<HashMap<String, Cached>>,
    loaded: AtomicUsize,
    skipped: AtomicUsize,
    failed: AtomicUsize,
    rasterized: AtomicUsize,
}

impl ImageLoader {
    /// Yeni yükleyici. `client` belge indirmesiyle aynı olabilir; bağlantı
    /// havuzu ve TLS el sıkışması tek yerden yönetilir.
    pub fn new(client: reqwest::Client, referer: &str) -> Self {
        Self {
            client,
            referer: referer.to_string(),
            cache: Mutex::new(HashMap::new()),
            loaded: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            rasterized: AtomicUsize::new(0),
        }
    }

    /// Verilen URL'leri indirip çözer (tekilleştirilmiş, sınırlı eşzamanlı).
    ///
    /// Hata durumunda da önbelleğe yazılır: render aşamasında ağ erişimi
    /// yapılmadığı için aynı görsel tekrar tekrar denenmez.
    pub async fn prefetch(self: &Arc<Self>, urls: &[String]) {
        let mut pending: Vec<String> = urls.iter().filter(|u| !u.is_empty()).cloned().collect();
        pending.sort();
        pending.dedup();

        let mut set = tokio::task::JoinSet::new();
        let mut queue = pending.into_iter();
        for _ in 0..PREFETCH_CONCURRENCY {
            let Some(url) = queue.next() else { break };
            set.spawn(self.clone().fetch(url));
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((url, cached, rasterized)) = joined {
                if rasterized {
                    self.rasterized.fetch_add(1, Ordering::Relaxed);
                }
                self.store(url, cached);
            }
            if let Some(url) = queue.next() {
                set.spawn(self.clone().fetch(url));
            }
        }
    }

    /// Tek bir görseli indirir ve çözer. Üçüncü değer: SVG rasterize edildi mi?
    async fn fetch(self: Arc<Self>, url: String) -> (String, Cached, bool) {
        let (cached, rasterized) = self.fetch_inner(&url).await;
        (url, cached, rasterized)
    }

    async fn fetch_inner(&self, url: &str) -> (Cached, bool) {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return (Cached::Failed(Failure::Unsupported), false);
        }
        let Some((bytes, format)) = self.download(url).await else {
            return (Cached::Failed(Failure::Network), false);
        };
        if bytes.is_empty() {
            return (Cached::Failed(Failure::Network), false);
        }
        // İçerik-tipi güvenilmez olabilir: tanınmıyorsa sihirli baytlar denenir.
        let format = format
            .or_else(|| Format::sniff(&bytes))
            .unwrap_or(Format::Png);

        let (decoded, rasterized) = if format == Format::Svg {
            (rasterize_svg(&bytes), true)
        } else {
            (decode_raster(&bytes, format).map(Decoded::raster), false)
        };
        match decoded {
            Some(img) if too_small(&img.img) => (Cached::Skipped, rasterized),
            Some(img) => (Cached::Image(img), rasterized),
            None => (Cached::Failed(Failure::Decode), rasterized),
        }
    }

    /// Görseli indirir; (bayt, biçim) döner.
    async fn download(&self, url: &str) -> Option<(bytes::Bytes, Option<Format>)> {
        let request = self
            .client
            .get(url)
            .header(
                "Accept",
                "image/avif,image/webp,image/apng,image/*,*/*;q=0.8",
            )
            .header("Referer", &self.referer);
        let response = tokio::time::timeout(IMAGE_TIMEOUT, request.send())
            .await
            .ok()?
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(Format::from_content_type)
            .unwrap_or(None);
        let bytes = tokio::time::timeout(IMAGE_TIMEOUT, response.bytes())
            .await
            .ok()?
            .ok()?;
        Some((bytes, content_type))
    }

    fn store(&self, url: String, cached: Cached) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(url, cached);
        }
    }

    /// Önbellekten PDF elemanı üretir. Ağ erişimi yapmaz.
    ///
    /// Görsel önbellekte yoksa `Failed` döner; `prefetch` tüm görselleri
    /// bildiği için bu yol yalnızca programatik kullanımda (test) olur.
    pub fn element(&self, url: &str, max_width_mm: f64, max_height_mm: f64) -> ImageOutcome {
        let entry = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(url).cloned());
        let outcome = match entry {
            Some(Cached::Image(img)) => prepare(img, max_width_mm, max_height_mm),
            Some(Cached::Skipped) => ImageOutcome::Skipped,
            Some(Cached::Failed(reason)) => ImageOutcome::Failed(reason),
            None => ImageOutcome::Failed(Failure::Network),
        };
        match &outcome {
            ImageOutcome::Loaded(_) => self.loaded.fetch_add(1, Ordering::Relaxed),
            ImageOutcome::Skipped => self.skipped.fetch_add(1, Ordering::Relaxed),
            ImageOutcome::Failed(_) => self.failed.fetch_add(1, Ordering::Relaxed),
        };
        outcome
    }

    /// Sayaçları okur.
    pub fn stats(&self) -> ImageStats {
        ImageStats {
            loaded: self.loaded.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            rasterized: self.rasterized.load(Ordering::Relaxed),
        }
    }

    /// Önbellekteki görsel sayısı (test/hata ayıklama).
    pub fn cached(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }
}

/// Görsel dekoratif mi? (izleyici pikseli, ikon, ayraç)
fn too_small(img: &image::DynamicImage) -> bool {
    let (width, height) = img.dimensions();
    width < MIN_PIXELS || height < MIN_PIXELS
}

/// Çözülmüş görseli PDF elemanına dönüştürür: alfa beyaza düzleştirilir,
/// alan ölçüsüne göre ölçeklenir.
fn prepare(decoded: Decoded, max_width_mm: f64, max_height_mm: f64) -> ImageOutcome {
    let rgb = flatten_alpha(decoded.img.as_ref());
    let (native_w, native_h) = decoded.natural_mm();
    let k = fit_scale(native_w, native_h, max_width_mm, max_height_mm);

    match GenImage::from_dynamic_image(rgb) {
        Ok(elem) => ImageOutcome::Loaded(Box::new(
            // `dpi` doğal ölçüyü belirler: SVG rasteri 2.1 kat büyütülmüş olduğu
            // için buraya 96 yazmak görseli PDF'te 2.1 kat büyük basardı.
            elem.with_dpi(decoded.dpi)
                .with_scale(Scale::new(k, k))
                .with_alignment(genpdf::Alignment::Center),
        )),
        Err(_) => ImageOutcome::Failed(Failure::Decode),
    }
}

/// Alfa kanalını beyaz zemin üzerine bindirir (RGBA/RGBA-palette PNG'ler).
/// Alfa yoksa görsel olduğu gibi döner.
fn flatten_alpha(img: &image::DynamicImage) -> image::DynamicImage {
    if !img.color().has_alpha() {
        return img.clone();
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

/// Baytları çözer. SVG bu yoldan geçmez (rasterize edilir).
fn decode_raster(bytes: &[u8], format: Format) -> Option<image::DynamicImage> {
    match format {
        Format::Webp => decode_webp(bytes),
        other => {
            let format = other.raster()?;
            image::io::Reader::with_format(std::io::Cursor::new(bytes.to_vec()), format)
                .decode()
                .ok()
        }
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

/// SVG'yi hedef çözünürlükte rasterize eder.
///
/// Fontlar gömülü fontlardan kurulur (sistem font taraması yapılmaz): hem
/// hızlı hem de platformdan bağımsız sonuç verir.
fn rasterize_svg(bytes: &[u8]) -> Option<Decoded> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(
        bytes,
        &usvg::Options {
            fontdb: svg_font_db(),
            ..Default::default()
        },
    )
    .ok()?;
    let size = tree.size();
    let (source_w, source_h) = (f64::from(size.width()), f64::from(size.height()));
    if source_w <= 0.0 || source_h <= 0.0 {
        return None;
    }
    let scale = raster_scale(source_w, source_h)?;
    let width = ((source_w * f64::from(scale)).round() as u32).max(1);
    let height = ((source_h * f64::from(scale)).round() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let rgba = pixmap.take_demultiplied();
    let img =
        image::RgbaImage::from_raw(width, height, rgba).map(image::DynamicImage::ImageRgba8)?;
    Some(Decoded {
        img: Arc::new(img),
        // Raster doğal boyutun bu katı olduğu için çözünürlük de o kadar artar.
        dpi: SCREEN_DPI * f64::from(scale),
    })
}

/// SVG için raster ölçeği.
///
/// Doğal ölçeğin (`SVG_RASTER_SCALE`) üzerine çıkılmaz, ama en uzun kenar ve
/// toplam piksel sınırına uyacak kadar küçültülür. Aşırı büyük çizimler
/// reddedilir (bellek koruması).
fn raster_scale(width_px: f64, height_px: f64) -> Option<f32> {
    if !(width_px > 0.0 && height_px > 0.0) {
        return None;
    }
    let longest = width_px.max(height_px);
    let mut scale = SVG_RASTER_SCALE.min(SVG_MAX_EDGE_PX / longest);
    if width_px * scale * height_px * scale > SVG_MAX_PIXELS {
        // Boyut sınırı da yetmiyorsa kalan bütçeye göre küçült.
        scale = (SVG_MAX_PIXELS / (width_px * height_px)).sqrt().min(scale);
    }
    (scale > 0.0).then_some(scale as f32)
}

/// Gömülü fontlardan kurulan SVG font veritabanı (sistem fontu taraması yok).
fn svg_font_db() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: std::sync::OnceLock<Arc<resvg::usvg::fontdb::Database>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_font_data(crate::pdf::SANS_FALLBACK.to_vec());
        db.load_font_data(crate::pdf::MONO_FALLBACK.to_vec());
        db.set_sans_serif_family("DejaVu Sans");
        db.set_monospace_family("DejaVu Sans Mono");
        Arc::new(db)
    })
    .clone()
}

/// Testler için: verilen baytları önbelleğe yazıp yükleyici üretir.
#[cfg(test)]
pub fn loader_with(entries: Vec<(String, Vec<u8>, Format)>) -> Arc<ImageLoader> {
    let loader = Arc::new(ImageLoader::new(
        reqwest::Client::new(),
        "https://example.com/",
    ));
    for (url, bytes, format) in entries {
        let decoded = if format == Format::Svg {
            rasterize_svg(&bytes)
        } else {
            decode_raster(&bytes, format).map(Decoded::raster)
        };
        let cached = match decoded {
            Some(img) if too_small(&img.img) => Cached::Skipped,
            Some(img) => Cached::Image(img),
            None => Cached::Failed(Failure::Decode),
        };
        loader.store(url, cached);
    }
    loader
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 200x90 CSS pikseli bir SVG, doğal ölçekte ~52.9x23.8 mm basılmalı
    /// (96 dpi varsayımı). Raster 2.1 kat büyütülse bile taşıyıcı çözünürlük
    /// buna göre raporlanmalı.
    #[test]
    fn svg_raster_keeps_its_natural_size() {
        const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 90" width="200" height="90"><rect width="200" height="90" fill="#000"/></svg>"##;
        let decoded = rasterize_svg(SVG.as_bytes()).expect("svg rasterize");
        let (w, h) = decoded.natural_mm();
        assert!((w - 52.9).abs() < 0.5, "genişlik {w} mm");
        assert!((h - 23.8).abs() < 0.5, "yükseklik {h} mm");
        // Raster çözünürlüğü doğal boyutun katı olmalı (200 dpi'ye yakın).
        assert!(
            decoded.dpi > 190.0 && decoded.dpi < 215.0,
            "{}",
            decoded.dpi
        );
    }

    #[test]
    fn svg_raster_is_capped_and_rejects_absurd_sizes() {
        // Normal bir şema: en uzun kenar sınırının altında.
        let scale = raster_scale(448.0, 512.0).expect("ölçek");
        assert!(f64::from(scale) > 1.0);
        // devasa viewBox: kenar sınırına küçültülür.
        let scale = raster_scale(200_000.0, 200_000.0).expect("ölçek");
        assert!(f64::from(scale) <= 2400.0 / 200_000.0 + 1e-6);
        // Sıfır ölçü geçersiz.
        assert!(raster_scale(0.0, 10.0).is_none());
    }

    #[test]
    fn svg_text_is_rasterized_with_embedded_fonts() {
        // SVG metni gömülü fontlarla çizilir (Türkçe karakterler dahil).
        const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 60" width="200" height="60"><rect width="200" height="60" fill="#fff"/><text x="10" y="40" font-family="DejaVu Sans" font-size="20" fill="#000">Merhaba ğüşıöç</text></svg>"##;
        let decoded = rasterize_svg(SVG.as_bytes()).expect("svg rasterize");
        let rgb = decoded.img.to_rgb8();
        let dark = rgb
            .pixels()
            .filter(|p| p.0.iter().any(|c| *c < 100))
            .count();
        assert!(dark > 500, "metin çizilmemiş (koyu piksel: {dark})");
    }

    #[test]
    fn tiny_svgs_are_treated_as_decorative() {
        const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8" width="8" height="8"><rect width="8" height="8" fill="#000"/></svg>"##;
        let decoded = rasterize_svg(SVG.as_bytes()).expect("svg rasterize");
        assert!(too_small(&decoded.img));
    }

    #[test]
    fn format_is_detected_from_content_type_and_magic_bytes() {
        assert_eq!(Format::from_content_type("image/PNG"), Some(Format::Png));
        assert_eq!(
            Format::from_content_type("image/jpeg; charset=binary"),
            Some(Format::Jpeg)
        );
        assert_eq!(Format::from_content_type("image/webp"), Some(Format::Webp));
        assert_eq!(Format::from_content_type("image/gif"), Some(Format::Gif));
        assert_eq!(
            Format::from_content_type("image/svg+xml"),
            Some(Format::Svg)
        );
        assert_eq!(Format::from_content_type("application/octet-stream"), None);

        assert_eq!(
            Format::sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some(Format::Png)
        );
        assert_eq!(Format::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(Format::sniff(b"GIF89a...."), Some(Format::Gif));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8L");
        assert_eq!(Format::sniff(&webp), Some(Format::Webp));
        // XML prologlu SVG de tanınır.
        assert_eq!(
            Format::sniff(b"<?xml version=\"1.0\"?><svg xmlns=\"x\"></svg>"),
            Some(Format::Svg)
        );
        assert_eq!(Format::sniff(b"junk"), None);
        // AVIF gibi desteklenmeyen biçimler bilinçli olarak yok sayılır.
        assert_eq!(
            Format::sniff(&[0, 0, 0, 0x20, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f']),
            None
        );
    }

    #[test]
    fn gif_images_are_decoded() {
        // GIF artık destekleniyor: küçük bir GIF üretip çözüyoruz.
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(40, 30, image::Rgb([1, 2, 3])))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Gif)
            .expect("gif kodlanmalı");
        let img = decode_raster(&out, Format::Gif).expect("gif çözülmeli");
        assert_eq!(image::GenericImageView::dimensions(&img), (40, 30));
    }

    #[test]
    fn loader_reports_outcomes_from_cache_without_network() {
        let png = png_bytes(60, 40);
        let loader = loader_with(vec![
            ("https://x/a.png".to_string(), png.clone(), Format::Png),
            (
                "https://x/kucuk.png".to_string(),
                png_bytes(4, 4),
                Format::Png,
            ),
            (
                "https://x/bozuk.png".to_string(),
                b"junk".to_vec(),
                Format::Png,
            ),
        ]);
        assert_eq!(loader.cached(), 3);
        assert!(matches!(
            loader.element("https://x/a.png", 170.0, 200.0),
            ImageOutcome::Loaded(_)
        ));
        assert!(matches!(
            loader.element("https://x/kucuk.png", 170.0, 200.0),
            ImageOutcome::Skipped
        ));
        assert!(matches!(
            loader.element("https://x/bozuk.png", 170.0, 200.0),
            ImageOutcome::Failed(Failure::Decode)
        ));
        // Önbellekte olmayan görsel: ağa çıkmadan başarısız sayılır.
        assert!(matches!(
            loader.element("https://x/yok.png", 170.0, 200.0),
            ImageOutcome::Failed(_)
        ));
        let stats = loader.stats();
        assert_eq!(stats.loaded, 1);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.failed, 2);
    }

    /// Gerçek indirme akışı: paylaşılan istemci, `Referer`, SVG rasterizasyonu
    /// ve hata durumlarının önbelleğe yazılması.
    #[tokio::test]
    async fn prefetch_downloads_images_and_sends_the_page_as_referer() {
        use crate::testutil::{header_of, path_of, Response, TestServer};
        const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 90" width="200" height="90"><rect width="200" height="90" fill="#f6f8fa"/></svg>"##;
        let png = png_bytes(80, 60);
        // Sıcak korumalı CDN'ler (GitHub camo, Cloudinary) `Referer` olmadan
        // görselleri reddeder; bu yüzden başlığı yakalayıp doğruluyoruz.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let server = TestServer::start(move |request| {
            recorder.lock().expect("kilit").push(request.to_string());
            match path_of(request).as_str() {
                path if path.ends_with("diagram.svg") => Response::binary("image/svg+xml", SVG),
                path if path.ends_with("foto.png") => Response::binary("image/png", png.clone()),
                _ => Response::status(404),
            }
        });
        let client = crate::fetch::build_client(crate::fetch::DEFAULT_TIMEOUT).unwrap();
        let loader = Arc::new(ImageLoader::new(client, "https://sayfa.example/yazi"));
        loader
            .prefetch(&[
                server.url("diagram.svg"),
                server.url("foto.png"),
                server.url("yok.png"),
            ])
            .await;

        assert_eq!(loader.cached(), 3);
        assert!(matches!(
            loader.element(&server.url("diagram.svg"), 170.0, 200.0),
            ImageOutcome::Loaded(_)
        ));
        assert!(matches!(
            loader.element(&server.url("foto.png"), 170.0, 200.0),
            ImageOutcome::Loaded(_)
        ));
        assert!(matches!(
            loader.element(&server.url("yok.png"), 170.0, 200.0),
            ImageOutcome::Failed(Failure::Network)
        ));
        let stats = loader.stats();
        assert_eq!(stats.loaded, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.rasterized, 1, "SVG rasterize edildi");

        let requests = seen.lock().expect("kilit");
        assert_eq!(requests.len(), 3, "her görsel bir kez indirilmeli");
        for request in requests.iter() {
            assert_eq!(
                header_of(request, "Referer"),
                Some("https://sayfa.example/yazi")
            );
            assert!(
                header_of(request, "Accept").is_some_and(|value| value.contains("image/")),
                "Accept başlığı eksik: {request}"
            );
        }
    }

    /// Aynı görsel iki kez indirilmemeli (tekilleştirme).
    #[tokio::test]
    async fn prefetch_deduplicates_urls() {
        use crate::testutil::{path_of, Response, TestServer};
        let png = png_bytes(80, 60);
        let hits = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&hits);
        let server = TestServer::start(move |request| {
            if path_of(request).ends_with("a.png") {
                *counter.lock().expect("kilit") += 1;
                Response::binary("image/png", png.clone())
            } else {
                Response::status(404)
            }
        });
        let client = crate::fetch::build_client(crate::fetch::DEFAULT_TIMEOUT).unwrap();
        let loader = Arc::new(ImageLoader::new(client, "https://sayfa.example/"));
        loader
            .prefetch(&[server.url("a.png"), server.url("a.png")])
            .await;
        assert_eq!(*hits.lock().expect("kilit"), 1);
        assert_eq!(loader.cached(), 1);
    }

    #[test]
    fn non_http_urls_are_never_fetched() {
        assert!(matches!(
            loader_with(vec![]).element("data:image/png;base64,AAA", 10.0, 10.0),
            ImageOutcome::Failed(_)
        ));
    }

    #[test]
    fn host_label_is_short_and_readable() {
        assert_eq!(
            host_label("https://cdn.example.com/a/b/diagram.png?x=1"),
            "cdn.example.com/diagram.png"
        );
        assert_eq!(host_label("not a url"), "not a url");
    }

    /// Test görselleri için küçük PNG üretir.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 9, 9])))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png kodlanmalı");
        out
    }
}
