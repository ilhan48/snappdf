//! Testler için minik HTTP/1.1 sunucusu.
//!
//! `reqwest`'i gerçek bir ağ olmadan sınamak için bağımlılıksız bir TCP
//! sunucusu: her bağlantıda tek istek okur, `Connection: close` ile kapanır.
//! Gövde `Vec<u8>` olduğundan ikili dosyalar (PNG/SVG) da bozulmadan servis
//! edilebilir.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Sunucunun bir isteğe verdiği yanıt.
pub struct Response {
    /// HTTP durum kodu.
    pub status: u16,
    /// `Content-Type` başlığı.
    pub content_type: String,
    /// Ham gövde (ikili olabilir).
    pub body: Vec<u8>,
}

impl Response {
    /// 200 + HTML gövdesi.
    pub fn html(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            body: body.into().into_bytes(),
        }
    }

    /// İstenen içerik tipiyle 200 gövdesi.
    pub fn binary(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: content_type.to_string(),
            body: body.into(),
        }
    }

    /// Hata kodu (gövdesi boş).
    pub fn status(code: u16) -> Self {
        Self {
            status: code,
            content_type: "text/plain".to_string(),
            body: Vec::new(),
        }
    }

    /// `Content-Type` değiştirir.
    pub fn with_type(mut self, content_type: &str) -> Self {
        self.content_type = content_type.to_string();
        self
    }
}

/// Test sunucusu.
pub struct TestServer {
    addr: String,
    handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl TestServer {
    /// Yanıtlayıcı ile sunucuyu başlatır. Yanıtlayıcı isteğin **tüm başlık
    /// satırını** alır (yol + `Referer` vb. doğrulaması için).
    pub fn start(responder: impl Fn(&str) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test sunucusu bağlanamadı");
        let addr = listener.local_addr().expect("adres alınamadı").to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).expect("non-blocking");
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 16 * 1024];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..read]).into_owned();
                        let response = responder(&request);
                        let head = format!(
                            "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            response.status,
                            response.content_type,
                            response.body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&response.body);
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
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

    /// İstek yolundan mutlak URL üretir (`/a.png` ya da `a.png`).
    pub fn url(&self, path: &str) -> String {
        format!("http://{}/{}", self.addr, path.trim_start_matches('/'))
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// İstekten yol kısmını çıkarır (`GET /a.png HTTP/1.1` -> `/a.png`).
pub fn path_of(request: &str) -> String {
    request.split_whitespace().nth(1).unwrap_or("/").to_string()
}

/// İstekte bir başlığın değerini döndürür (bulunamazsa `None`).
pub fn header_of<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}
/// Üretilen PDF'ten **gerçek metni** çıkarır (ToUnicode CMap üzerinden).
///
/// `lopdf::Document::extract_text` Identity-H kodlamalı CID fontları
/// çözemez (fontun `/Encoding`ine bakar, ToUnicode'a değil), bu yüzden
/// metin seçilebilirliğini sınamak için gereklidir.
///
/// `ActualText` ile boşaltılan öbekler (kod satır numaraları, dil rozeti)
/// atlanır — PDF okuyucuların seç-kopyala davranışıyla aynı sonucu verir.
pub fn extract_text(bytes: &[u8]) -> String {
    use std::collections::HashMap;
    let doc = match lopdf::Document::load_mem(bytes) {
        Ok(doc) => doc,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    for page_id in doc.get_pages().values() {
        let fonts = page_fonts(&doc, *page_id);

        let Ok(raw) = doc.get_page_content(*page_id) else {
            continue;
        };
        // lopdf 0.26 `%` ile başlayan satırları (bizim işaret yorumlarımızı)
        // çözemiyor ve akışın tamamını boş döndürüyor; yorumlar PDF'te
        // anlamsızdır, bu yüzden sadece test için ayıklanır.
        let content = match lopdf::content::Content::decode(&strip_comments(&raw)) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let mut current: Option<&HashMap<u16, char>> = None;
        // ActualText boşaltılmış alanın derinliği.
        let mut hidden_depth = 0usize;
        for operation in &content.operations {
            match operation.operator.as_ref() {
                "BDC" | "BMC" => {
                    if is_hidden_mark(&operation.operands) {
                        hidden_depth += 1;
                    }
                }
                "EMC" => hidden_depth = hidden_depth.saturating_sub(1),
                "Tf" => {
                    current = operation
                        .operands
                        .first()
                        .and_then(|object| object.as_name().ok())
                        .and_then(|name| fonts.get(name));
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    if hidden_depth == 0 {
                        if let Some(map) = current {
                            append_text(&mut out, &operation.operands, map);
                        }
                    }
                }
                "ET" if !out.ends_with('\n') => out.push('\n'),
                _ => {}
            }
        }
    }
    out
}

/// İçerik akışındaki `%` yorum satırlarını ayıklar (lopdf çözümleyicisi için).
fn strip_comments(content: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(content);
    if !text.contains('%') {
        return content.to_vec();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('%') {
            continue;
        }
        out.push_str(line);
    }
    out.into_bytes()
}

/// Sayfanın font adlarını ToUnicode eşlemeleriyle döndürür.
///
/// `lopdf::Document::get_page_fonts` `/Resources`ı yalnızca **doğrudan**
/// sözlükken bulur; printpdf ise onu dolaylı nesne olarak yazar. Bu yüzden
/// kaynaklar burada elle çözülür.
fn page_fonts(
    doc: &lopdf::Document,
    page_id: lopdf::ObjectId,
) -> std::collections::HashMap<Vec<u8>, std::collections::HashMap<u16, char>> {
    let mut out = std::collections::HashMap::new();
    let Some(resources) = resolve_dict(doc, page_id, b"Resources") else {
        return out;
    };
    let Some(font_dict) = resources
        .get(b"Font")
        .ok()
        .and_then(|object| resolve(doc, object))
    else {
        return out;
    };
    let Ok(font_dict) = font_dict.as_dict() else {
        return out;
    };
    for (name, value) in font_dict.iter() {
        let Some(font) = resolve(doc, value).and_then(|object| object.as_dict().ok()) else {
            continue;
        };
        let Some(cmap) = font
            .get(b"ToUnicode")
            .ok()
            .and_then(|object| resolve(doc, object))
        else {
            continue;
        };
        let Ok(stream) = cmap.as_stream() else {
            continue;
        };
        let data = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        out.insert(name.clone(), parse_to_unicode(&data));
    }
    out
}

/// Bir sözlüğün anahtarını, dolaylı referansları çözerek getirir.
fn resolve_dict<'a>(
    doc: &'a lopdf::Document,
    id: lopdf::ObjectId,
    key: &[u8],
) -> Option<&'a lopdf::Dictionary> {
    let dict = doc.get_object(id).ok()?.as_dict().ok()?;
    resolve(doc, dict.get(key).ok()?).and_then(|object| object.as_dict().ok())
}

/// Dolaylı referansı nesneye indirger.
fn resolve<'a>(doc: &'a lopdf::Document, object: &'a lopdf::Object) -> Option<&'a lopdf::Object> {
    match object {
        lopdf::Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// `BDC` operandları boş bir `ActualText` işaretini gösteriyor mu?
fn is_hidden_mark(operands: &[lopdf::Object]) -> bool {
    operands.iter().any(|operand| match operand {
        lopdf::Object::Dictionary(dict) => dict
            .get(b"ActualText")
            .map(|text| matches!(text, lopdf::Object::String(bytes, _) if bytes.is_empty()))
            .unwrap_or(false),
        _ => false,
    })
}

/// Metin gösterim operanlarını ToUnicode eşlemesiyle çözer.
fn append_text(
    out: &mut String,
    operands: &[lopdf::Object],
    map: &std::collections::HashMap<u16, char>,
) {
    for operand in operands {
        match operand {
            lopdf::Object::String(bytes, _) => {
                for pair in bytes.chunks(2) {
                    let code = if pair.len() == 2 {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from(pair[0])
                    };
                    if let Some(ch) = map.get(&code) {
                        out.push(*ch);
                    }
                }
            }
            lopdf::Object::Array(items) => append_text(out, items, map),
            _ => {}
        }
    }
}

/// ToUnicode CMap'in `beginbfchar` bloklarını `glyph -> char` eşlemesine çevirir.
fn parse_to_unicode(data: &[u8]) -> std::collections::HashMap<u16, char> {
    let text = String::from_utf8_lossy(data);
    let mut map = std::collections::HashMap::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.trim();
        if line.ends_with("beginbfchar") {
            in_block = true;
            continue;
        }
        if line.starts_with("endbfchar") {
            in_block = false;
            continue;
        }
        if !in_block {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(source), Some(target)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(code) = hex_u16(source) else {
            continue;
        };
        if let Some(ch) = hex_string_to_char(target) {
            map.insert(code, ch);
        }
    }
    map
}

/// `<00e4>` -> `0x00e4`.
fn hex_u16(token: &str) -> Option<u16> {
    let trimmed = token.trim_start_matches('<').trim_end_matches('>');
    u16::from_str_radix(&trimmed[..trimmed.len().min(4)], 16).ok()
}

/// `<0041>` veya vekil çiftli `<d83dde00>` -> `'A'` / `'😀'`.
fn hex_string_to_char(token: &str) -> Option<char> {
    let trimmed = token.trim_start_matches('<').trim_end_matches('>');
    let units: Vec<u16> = trimmed
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .filter_map(|chunk| {
            std::str::from_utf8(chunk)
                .ok()
                .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        })
        .collect();
    match units.as_slice() {
        [single] => char::from_u32(u32::from(*single)),
        [high, low] => char::decode_utf16([*high, *low]).next()?.ok(),
        _ => None,
    }
}
