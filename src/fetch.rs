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

    use crate::testutil::{path_of, Response, TestServer};

    #[tokio::test]
    async fn fetch_html_success() {
        let server = TestServer::start(|request| {
            if path_of(request).contains("ok") {
                Response::html("<html><body>merhaba dünya</body></html>")
            } else {
                Response::status(404)
            }
        });
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let html = fetch_html(&client, &server.url("ok")).await.unwrap();
        assert!(html.contains("merhaba"));
    }

    #[tokio::test]
    async fn fetch_html_404_is_fatal() {
        let server = TestServer::start(|_| Response::status(404));
        let client = build_client(DEFAULT_TIMEOUT).unwrap();
        let err = fetch_html(&client, &server.url("yok")).await.unwrap_err();
        assert!(err.to_string().contains("404"));
    }

    #[tokio::test]
    async fn fetch_html_non_html_content_rejected() {
        let server = TestServer::start(|_| Response::html("%PDF-1.4").with_type("application/pdf"));
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
}
