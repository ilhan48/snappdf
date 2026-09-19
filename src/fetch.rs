use anyhow::{bail, Context, Result};
use std::time::Duration;

/// Varsayılan zaman aşımı.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// snappdf'in kullandığı User-Agent.
pub const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (compatible; snappdf/0.2; +https://github.com/snappdf)";

/// Deneme sayısı (ilk istek + 2 tekrar).
pub const MAX_ATTEMPTS: u32 = 3;

/// Yapılandırılabilir HTTP istemcisi; testlerde local sunucuya işaret edilir.
pub fn build_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT_VALUE)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("HTTP istemcisi oluşturulamadı")
}

/// Yanıtın metin gövdesini döndürür; UTF-8 dışıysa kayıpsız çevrim denenir.
pub fn body_to_string(content_type: Option<&str>, bytes: &[u8]) -> String {
    let is_utf8 = std::str::from_utf8(bytes).is_ok();
    let charset_is_utf8 = content_type
        .map(|ct| ct.to_ascii_lowercase().contains("utf-8"))
        .unwrap_or(false);
    if is_utf8 || charset_is_utf8 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    // ISO-8859-1 (latin-1), UTF-8 olmayan sık içerik-tipi: bayt-bayta eşlenir.
    bytes.iter().map(|&b| b as char).collect()
}

/// Bir URL'yi HTML metni olarak indirir.
/// - 2xx olmayan durum kodları hata döner
/// - HTML olmayan içerik tipleri reddedilir
/// - Ağ hatalarında üstel beklemeyle MAX_ATTEMPTS kez denenir
pub async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match attempt_once_html(client, url).await {
            Ok(text) => return Ok(text),
            Err(retryable @ FetchError::Network(_)) => {
                last_err = retryable.to_string();
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                }
            }
            Err(FetchError::Fatal(e)) => return Err(e),
        }
    }
    bail!("{url} indirilemedi ({MAX_ATTEMPTS} deneme): {last_err}")
}

enum FetchError {
    Network(String),
    Fatal(anyhow::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(m) => write!(f, "ağ hatası: {m}"),
            FetchError::Fatal(e) => write!(f, "{e}"),
        }
    }
}

async fn attempt_once_html(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<String, FetchError> {
    let resp = client
        .get(url)
        .header("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8")
        .send()
        .await
        .map_err(|e| FetchError::Network(format!("{e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Fatal(anyhow::anyhow!(
            "sunucu {status} döndürdü: {url}"
        )));
    }

    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !ctype.is_empty()
        && !ctype.contains("html")
        && !ctype.contains("xml")
        && !ctype.contains("text/plain")
    {
        return Err(FetchError::Fatal(anyhow::anyhow!(
            "HTML olmayan içerik tipi '{ctype}': {url}"
        )));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| FetchError::Network(format!("{e}")))?;
    let ct = if ctype.is_empty() {
        None
    } else {
        Some(ctype.as_str())
    };
    Ok(body_to_string(ct, &bytes))
}

/// Bir görseli indirir; (veri, format) döner. Başarısızlıkta None döner —
/// bir görsel PDF üretimini asla engellememeli.
pub async fn fetch_image(
    client: &reqwest::Client,
    url: &str,
) -> Option<(bytes::Bytes, &'static str)> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let data = resp.bytes().await.ok()?;
    if data.is_empty() {
        return None;
    }
    // Önce içerik-tipi, tanınmıyorsa sihirli baytlar (bazı sunucular
    // application/octet-stream döndürür). gif/svg PDF'e gömülmez.
    let by_ctype = if ctype.contains("png") {
        Some("png")
    } else if ctype.contains("jpeg") || ctype.contains("jpg") {
        Some("jpeg")
    } else if ctype.contains("webp") {
        Some("webp")
    } else {
        None
    };
    let format = by_ctype.or_else(|| sniff_image_format(&data))?;
    Some((data, format))
}

/// İçerik-tipi güvenilmezse sihirli baytlardan biçim çıkarır.
pub fn sniff_image_format(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("png");
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpeg");
    }
    // RIFF....WEBP
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// Mutlak URL üretir: baz URL'ye göre çözer; zaten mutlaksa olduğu gibi döner.
pub fn absolute_url(base: &str, href: &str) -> Option<String> {
    if href.is_empty() || href.starts_with("data:") || href.starts_with("javascript:") {
        return None;
    }
    if let Ok(abs) = reqwest::Url::parse(href) {
        if abs.scheme().starts_with("http") {
            return Some(abs.to_string());
        }
        return None;
    }
    let base_url = reqwest::Url::parse(base).ok()?;
    base_url.join(href).ok().map(|u| u.to_string())
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Testler için minik TCP tabanlı HTTP sunucusu (bağımlılık yok).
    struct TestServer {
        addr: String,
        handle: Option<std::thread::JoinHandle<()>>,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl TestServer {
        fn start(responder: impl Fn(&str) -> String + Send + 'static) -> Self {
            use std::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let sd = shutdown.clone();
            let handle = std::thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !sd.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut buf = [0u8; 4096];
                            let n = stream.read(&mut buf).unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                            let body = responder(&path);
                            let status_line = if body.starts_with("HTTP_ERR ") {
                                let code = body
                                    .trim_start_matches("HTTP_ERR ")
                                    .split_whitespace()
                                    .next()
                                    .unwrap_or("500");
                                format!("HTTP/1.1 {code} Err\r\n")
                            } else {
                                "HTTP/1.1 200 OK\r\n".to_string()
                            };
                            let _body_out = body
                                .trim_start_matches("HTTP_ERR 500 ")
                                .trim_start_matches("HTTP_ERR 404 ")
                                .to_string();
                            let hdrs = if body.starts_with("HTTP_ERR") {
                                String::new()
                            } else if body.starts_with("CTYPE:") {
                                let ct = body
                                    .trim_start_matches("CTYPE:")
                                    .split("|NEXT|")
                                    .next()
                                    .unwrap_or("text/html");
                                format!("Content-Type: {ct}\r\n")
                            } else {
                                "Content-Type: text/html; charset=utf-8\r\n".to_string()
                            };
                            let payload = body
                                .trim_start_matches("CTYPE:")
                                .split("|NEXT|")
                                .last()
                                .unwrap_or("")
                                .to_string();
                            let resp = format!("{status_line}{hdrs}Content-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload);
                            let _ = stream.write_all(resp.as_bytes());
                            let _ = stream.flush();
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                addr,
                handle: Some(handle),
                shutdown,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}/{}", self.addr, path.trim_start_matches('/'))
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    #[tokio::test]
    async fn fetch_html_success() {
        let server = TestServer::start(|path| {
            if path.contains("ok") {
                "<html><body>merhaba dünya</body></html>".to_string()
            } else {
                "HTTP_ERR 404 bulunamadı".to_string()
            }
        });
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let html = fetch_html(&client, &server.url("ok")).await.unwrap();
        assert!(html.contains("merhaba"));
    }

    #[tokio::test]
    async fn fetch_html_404_is_fatal() {
        let server = TestServer::start(|_| "HTTP_ERR 404 bulunamadı".to_string());
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let err = fetch_html(&client, &server.url("yok")).await.unwrap_err();
        assert!(err.to_string().contains("404"));
    }

    #[tokio::test]
    async fn fetch_html_non_html_content_rejected() {
        let server = TestServer::start(|_| "CTYPE: application/pdf|NEXT|%PDF-1.4".to_string());
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let err = fetch_html(&client, &server.url("doc.pdf"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTML olmayan içerik tipi"));
    }

    #[tokio::test]
    async fn fetch_html_unreachable_retries_then_fails() {
        // Bağlantının reddedileceği garanti bir port: 1 (ayrıcalıklı, kapalı).
        let client = build_client(Duration::from_secs(2)).unwrap();
        let err = fetch_html(&client, "http://127.0.0.1:9/")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("indirilemedi"));
    }

    #[test]
    fn body_to_string_utf8_passthrough() {
        let s = "ığüşöç".as_bytes().to_vec();
        assert_eq!(
            body_to_string(Some("text/html; charset=utf-8"), &s),
            "ığüşöç"
        );
        assert_eq!(body_to_string(None, &s), "ığüşöç");
    }

    #[test]
    fn body_to_string_latin1_fallback() {
        // 0xE7 = 'ç' ISO-8859-1'de; geçerli UTF-8 değil.
        let bytes = vec![0xE7, 0x61];
        let out = body_to_string(Some("text/html; charset=iso-8859-1"), &bytes);
        assert_eq!(out, "ça");
    }

    #[test]
    fn user_agent_and_limits_are_set() {
        assert!(USER_AGENT_VALUE.contains("snappdf"));
        assert_eq!(MAX_ATTEMPTS, 3);
    }

    #[test]
    fn absolute_url_handles_all_cases() {
        assert_eq!(
            absolute_url("https://a.com/x/y", "/img.png").as_deref(),
            Some("https://a.com/img.png")
        );
        assert_eq!(
            absolute_url("https://a.com/x/", "b.png").as_deref(),
            Some("https://a.com/x/b.png")
        );
        assert_eq!(
            absolute_url("https://a.com", "https://b.com/c").as_deref(),
            Some("https://b.com/c")
        );
        assert!(absolute_url("https://a.com", "").is_none());
        assert!(absolute_url("https://a.com", "data:image/png;base64,xx").is_none());
        assert!(absolute_url("https://a.com", "javascript:void(0)").is_none());
        assert!(absolute_url("https://a.com", "ftp://f/x").is_none());
        assert!(absolute_url("not a base", "x").is_none());
    }

    #[tokio::test]
    async fn fetch_image_accepts_png_and_rejects_other() {
        let png1x1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let server = TestServer::start(move |path| {
            if path.contains("resim.png") {
                // İçerik-tipi başlığı + gövde; TestServer protokolü: CTYPE:<ct>|NEXT|<gövde>
                format!("CTYPE: image/png|NEXT|{}", String::from_utf8_lossy(png1x1))
            } else if path.contains("foto.webp") {
                "CTYPE: image/webp|NEXT|RIFF....WEBP".to_string()
            } else {
                "CTYPE: image/svg+xml|NEXT|<svg/>".to_string()
            }
        });
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let got = fetch_image(&client, &server.url("resim.png")).await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().1, "png");

        // WebP artık kabul edilir (kayıpsız/alfalı WebP'ler yaygın).
        let webp = fetch_image(&client, &server.url("foto.webp")).await;
        assert_eq!(webp.unwrap().1, "webp");

        // SVG hâlâ reddedilir (tarayıcısız rasterleştirme yok).
        let rejected = fetch_image(&client, &server.url("vektor.svg")).await;
        assert!(rejected.is_none());
    }

    #[test]
    fn sniffs_webp_from_magic_bytes() {
        let mut data = b"RIFF".to_vec();
        data.extend_from_slice(&[0x24, 0x00, 0x00, 0x00]);
        data.extend_from_slice(b"WEBPVP8L");
        assert_eq!(sniff_image_format(&data), Some("webp"));
        // Eksik/sahte başlıklar WebP sayılmaz.
        assert_eq!(sniff_image_format(b"RIFF"), None);
        assert_eq!(sniff_image_format(b"RIFF____XXXX"), None);
    }
}
