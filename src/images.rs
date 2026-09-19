use genpdf::elements::Image as GenImage;
use std::collections::HashMap;
use std::sync::Mutex;

/// Görsel önbelleği: aynı URL'yi bir render içinde iki kez indirmemek için.
static CACHE: Mutex<Option<HashMap<String, image::DynamicImage>>> = Mutex::new(None);

/// URL'den görsel indirir, çözümler ve genpdf Image elemanı üretir.
///
/// İndirme/çözümleme başarısızsa None döner — görsel hatası PDF'i engellememeli.
pub fn load_blocking(url: &str) -> Option<GenImage> {
    load_blocking_inner(url, || network_fetch(url), decode)
}

/// Test edilebilir çekirdek: `fetch` ve `decode` bağımlılıkları parametrik.
fn load_blocking_inner<F, D>(url: &str, fetch: F, decode_fn: D) -> Option<GenImage>
where
    F: FnOnce() -> Option<(bytes::Bytes, &'static str)>,
    D: FnOnce(bytes::Bytes, &str) -> Option<image::DynamicImage>,
{
    // Yalnızca http(s) desteklenir (data:/ftp: dışlanır).
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }

    if let Some(img) = cached(url) {
        return GenImage::from_dynamic_image(img).ok();
    }

    let (bytes, format) = fetch()?;
    let img = decode_fn(bytes, format)?;

    store(url, &img);
    GenImage::from_dynamic_image(img).ok()
}

/// Gerçek ağ üzerinden getiren varsayılan fetch.
fn network_fetch(url: &str) -> Option<(bytes::Bytes, &'static str)> {
    // pdf.rs senkron bağlamda çağırır; ağ için kısa ömürlü blok istek yapılır.
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::try_current().ok()?;
        rt.block_on(async {
            let client = crate::fetch::build_client(std::time::Duration::from_secs(10)).ok()?;
            crate::fetch::fetch_image(&client, url).await
        })
    })
}

#[cfg(test)]
fn load_blocking_with(
    url: &str,
    fetch: impl FnOnce() -> Option<(bytes::Bytes, &'static str)>,
    decode_fn: impl FnOnce(bytes::Bytes, &str) -> Option<image::DynamicImage>,
) -> Option<GenImage> {
    load_blocking_inner(url, fetch, decode_fn)
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
        _ => None,
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

#[cfg(test)]
use image::GenericImageView;

#[cfg(test)]
mod tests {
    use super::*;

    /// 1x1 kırmızı PNG.
    const RED_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn rejects_non_http_schemes() {
        // CACHE'e diğer testlerle yarışmadan dokunmak için kilidi al.
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        assert!(load_blocking("data:image/png;base64,AAA").is_none());
        assert!(load_blocking("ftp://x/y.png").is_none());
        assert!(load_blocking("").is_none());
    }

    #[test]
    fn decode_png_works_and_rejects_garbage() {
        let img = decode(bytes::Bytes::from_static(RED_PNG), "png");
        assert!(img.is_some());
        assert!(decode(bytes::Bytes::from_static(b"not an image"), "png").is_none());
        assert!(decode(bytes::Bytes::from_static(RED_PNG), "webp").is_none());
    }

    /// Statik CACHE test iş parçacıkları arasında paylaşılır; cache kullanan
    /// testler bu kilidi alarak serileştirir.
    static CACHE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn store_and_cached_roundtrip() {
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        let img = image::DynamicImage::new_rgb8(2, 2);
        assert!(cached("https://x/a.png").is_none());
        store("https://x/a.png", &img);
        let got = cached("https://x/a.png");
        assert!(got.is_some());
        assert_eq!(got.unwrap().dimensions(), (2, 2));
        clear_cache();
        assert!(cached("https://x/a.png").is_none());
    }

    #[test]
    fn cache_is_shared_and_independent_by_url() {
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        let a = image::DynamicImage::new_rgb8(3, 3);
        let b = image::DynamicImage::new_luma8(4, 4);
        store("https://y/1.png", &a);
        store("https://y/2.png", &b);
        assert_eq!(cached("https://y/1.png").unwrap().dimensions(), (3, 3));
        assert_eq!(cached("https://y/2.png").unwrap().dimensions(), (4, 4));
        clear_cache();
    }

    #[test]
    fn decoded_png_has_expected_dimensions() {
        let img = decode(bytes::Bytes::from_static(RED_PNG), "png").unwrap();
        assert_eq!(img.dimensions(), (1, 1));
    }

    #[test]
    fn load_blocking_fetches_decodes_and_caches() {
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        let mut fetch_count = 0usize;
        let url = "https://mock/img.png";

        let img = load_blocking_with(
            url,
            || {
                fetch_count += 1;
                Some((bytes::Bytes::from_static(RED_PNG), "png"))
            },
            decode,
        );
        assert!(img.is_some());
        assert_eq!(fetch_count, 1);

        // İkinci istek önbellekten gelmeli; fetch bir kez daha çalışmamalı.
        let again = load_blocking_with(
            url,
            || {
                fetch_count += 1;
                Some((bytes::Bytes::from_static(RED_PNG), "png"))
            },
            decode,
        );
        assert!(again.is_some());
        assert_eq!(fetch_count, 1, "önbellek isabeti ağa gitmamalı");
        clear_cache();
    }

    #[test]
    fn load_blocking_none_when_fetch_fails() {
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        let img = load_blocking_with("https://mock/broken.png", || None, decode);
        assert!(img.is_none());
        clear_cache();
    }

    #[test]
    fn load_blocking_none_when_decode_fails_but_fetch_ok() {
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        let img = load_blocking_with(
            "https://mock/garbage.png",
            || Some((bytes::Bytes::from_static(b"junk"), "png")),
            decode,
        );
        assert!(img.is_none());
        clear_cache();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_load_blocking_uses_network_fetch_inside_runtime() {
        // network_fetch yolu (block_in_place + Handle::try_current) gerçek
        // runtime içinde doğrulanır: mock sunucu üzerinden tam döngü.
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();

        // Minik HTTP sunucu: 1x1 PNG döner.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let body = RED_PNG;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        let url = format!("http://{addr}/real.png");
        let img = load_blocking(&url);
        assert!(img.is_some(), "gerçek döngüde görsel yüklenmeli");
        clear_cache();
        let _ = handle.join();
    }

    #[test]
    fn load_blocking_none_outside_runtime() {
        // Runtime olmayan bağlamda network_fetch None dönmeli (panik değil).
        let _guard = CACHE_LOCK.lock().unwrap();
        clear_cache();
        assert!(load_blocking("https://örnek-yok.example/a.png").is_none());
        clear_cache();
    }
}
