//! Google Translate entegrasyonu (`--tr`).
//!
//! Ücretsiz, anahtarsız Google endpoint'ini kullanır. Metni blok blok
//! gönderir; kod bloklarına ve görsellere asla dokunmaz. Satır içi korumalı
//! parçaları (`` `kod` ``, URL'ler, `<etiket>`ler) yer tutucuya sarıp çeviri
//! sonrası geri açar. Bir blok çevrilemezse **hiç dokunulmamış** orijinaliyle
//! kalır — çeviri PDF üretimini asla engellemez.

use anyhow::{bail, Context, Result};
use std::time::Duration;

/// Ücretsiz, anahtarsız Google Translate endpoint'i (`dj=1` JSON yanıtı).
const GOOGLE_ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";

/// Tek bir istekte gönderilecek metin üst sınırı (URL uzunluğu güvencesi).
const MAX_CHUNK_CHARS: usize = 3_000;

/// Google Translate'in dönüp durduğu ucun zaman aşımı.
const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(20);

/// Çeviri motoru; `run` başına bir kez kurulur.
pub struct Translator {
    client: reqwest::Client,
    target: String,
}

impl Translator {
    pub fn new(target: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(crate::fetch::USER_AGENT_VALUE)
            .timeout(TRANSLATE_TIMEOUT)
            .build()
            .context("çeviri istemcisi oluşturulamadı")?;
        Ok(Self {
            client,
            target: target.to_string(),
        })
    }

    /// Bir metni hedef dile çevirir. Korunan parçaları yer tutucuya sarar;
    /// Google yer tutucuları düşürürse orijinal metin döner.
    pub async fn translate(&self, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }
        let (masked, parts) = protect(text);
        if parts.is_empty() {
            return self.request(text).await;
        }
        let translated = self.request(&masked).await?;
        Ok(restore(&translated, &parts).unwrap_or_else(|| text.to_string()))
    }

    /// Metni parça parça gönderir (uzun paragraf URL sınırına takılmasın).
    async fn request(&self, text: &str) -> Result<String> {
        let mut out = String::new();
        for piece in chunk(text, MAX_CHUNK_CHARS) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&self.request_once(&piece).await?);
        }
        Ok(out)
    }

    /// Tek parça için iki deneme yapar (ücretsiz endpoint arada 429 döndürür).
    async fn request_once(&self, text: &str) -> Result<String> {
        let mut last_err = String::new();
        for attempt in 1..=2 {
            match self.try_once(text).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    last_err = e.to_string();
                    if attempt == 1 {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                }
            }
        }
        bail!("çeviri başarısız: {last_err}")
    }

    async fn try_once(&self, text: &str) -> Result<String> {
        // reqwest 0.13'te .query kaldırıldı; URL'yi url crate'iyle kurarız
        // (percent-encoding'i query_pairs_mut halleder).
        let mut url = reqwest::Url::parse(GOOGLE_ENDPOINT).context("çeviri URL'si bozuk")?;
        url.query_pairs_mut()
            .append_pair("client", "gtx")
            .append_pair("sl", "auto")
            .append_pair("tl", &self.target)
            .append_pair("dt", "t")
            .append_pair("dj", "1")
            .append_pair("q", text);
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("çeviri servisine ulaşılamadı")?;
        if !resp.status().is_success() {
            bail!("çeviri servisi {} döndürdü", resp.status());
        }
        let bytes = resp
            .bytes()
            .await
            .context("çeviri yanıtı alınamadı")?;
        let body: DjResponse =
            serde_json::from_slice(&bytes).context("çeviri yanıtı çözümlenemedi")?;
        let out = body
            .sentences
            .into_iter()
            .map(|s| s.trans)
            .collect::<Vec<_>>()
            .join("");
        if out.trim().is_empty() {
            bail!("çeviri servisi boş yanıt döndürdü");
        }
        Ok(out)
    }
}

/// `dj=1` JSON yanıtı: `{"sentences":[{"trans":"...","orig":"..."},...]}`
#[derive(serde::Deserialize)]
struct DjResponse {
    #[serde(default)]
    sentences: Vec<DjSentence>,
}

#[derive(serde::Deserialize)]
struct DjSentence {
    #[serde(default)]
    trans: String,
}

/// Metni boşluk sınırından parçalara böler; hiçbir metin kaybolmaz.
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > max {
            // Tek kelime bile sınırı aşıyor: karakterden böleriz.
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut piece = String::new();
            for c in word.chars() {
                piece.push(c);
                if piece.chars().count() >= max {
                    chunks.push(std::mem::take(&mut piece));
                }
            }
            if !piece.is_empty() {
                chunks.push(piece);
            }
            continue;
        }
        if current.chars().count() + word.chars().count() + 1 > max {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// `⟦N⟧` yer tutucusu: korunan parça N. dizine gider.
fn marker(idx: usize) -> String {
    format!("⟦{idx}⟧")
}

/// Kod/URL/etiket parçalarını yer tutucularla değiştirir; (maske, parçalar) döner.
fn protect(text: &str) -> (String, Vec<String>) {
    let mut masked = String::with_capacity(text.len());
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        // (önek, kalanda aranan bitiş) — eşleşen parça korumaya alınır.
        let span_end = if let Some(stripped) = rest.strip_prefix('`') {
            // `satır içi kod`
            stripped.find('`').map(|e| e + 2)
        } else if rest.starts_with("http://") || rest.starts_with("https://") {
            Some(url_span_end(rest))
        } else if rest.starts_with('<') {
            // Satır içi etiket: <b>, </i>, <code> ...
            rest.find('>').map(|e| e + 1)
        } else {
            None
        };
        if let Some(end) = span_end {
            let span = &rest[..end];
            // Tek karakterlik `<`/`` ` `` artıkları korumaya değmez.
            if span.chars().count() > 1 {
                parts.push(span.to_string());
                masked.push_str(&marker(parts.len() - 1));
                i += end;
                continue;
            }
        }
        let c = rest.chars().next().expect("boş dilim yok");
        masked.push(c);
        i += c.len_utf8();
    }
    (masked, parts)
}

/// URL'nin ilk boşluğa kadar uzanan koruma aralığı uzunluğu.
fn url_span_end(rest: &str) -> usize {
    rest.char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(rest.len())
}

/// Yer tutucuları parçalarla geri değiştirir. Bir yer tutucu bile eksikse
/// `None` döner — çağıran orijinal metni kullanır (kaybolan kod, yanlış
/// çevrilmiş cümleden iyidir).
fn restore(text: &str, parts: &[String]) -> Option<String> {
    let mut out = text.to_string();
    for (idx, part) in parts.iter().enumerate() {
        let m = marker(idx);
        if !out.contains(&m) {
            return None;
        }
        out = out.replace(&m, part);
    }
    Some(out)
}

// ------------------------------------------------------------------ makale

/// Hangi blok alanının çevrildiği.
#[derive(Clone, Copy)]
enum Kind {
    Paragraph,
    Heading,
    Caption,
    Quote,
    ListItem,
    TableHeader,
    TableCell,
}

struct Job {
    index: usize,
    kind: Kind,
    row: usize,
    col: usize,
    text: String,
}

/// Bir makaleyi blok blok çevirir: başlık, paragraf, başlık, açıklama,
/// alıntı, liste ögesi ve tablo hücreleri. Kod blokları, görseller ve
/// ayraçlara dokunulmaz. Başarısız bloklar orijinal kalır.
pub async fn translate_article(article: &mut crate::extract::Article, tr: &Translator) {
    // 1) Başlık.
    let title = article.title.clone();
    match tr.translate(&title).await {
        Ok(t) => article.title = t,
        Err(e) => eprintln!("[çeviri] başlık çevrilemedi: {e}"),
    }

    // 2) Çevrilecek metinleri topla (kod/görsel/ayraç korunur).
    use crate::extract::Block;
    let mut jobs: Vec<Job> = Vec::new();
    for (index, block) in article.blocks.iter().enumerate() {
        match block {
            Block::Paragraph(s) => jobs.push(Job {
                index,
                kind: Kind::Paragraph,
                row: 0,
                col: 0,
                text: s.clone(),
            }),
            Block::Heading { text, .. } => jobs.push(Job {
                index,
                kind: Kind::Heading,
                row: 0,
                col: 0,
                text: text.clone(),
            }),
            Block::Caption(s) => jobs.push(Job {
                index,
                kind: Kind::Caption,
                row: 0,
                col: 0,
                text: s.clone(),
            }),
            Block::Quote(s) => jobs.push(Job {
                index,
                kind: Kind::Quote,
                row: 0,
                col: 0,
                text: s.clone(),
            }),
            Block::ListItem { text, .. } => jobs.push(Job {
                index,
                kind: Kind::ListItem,
                row: 0,
                col: 0,
                text: text.clone(),
            }),
            Block::Table(t) => {
                for (col, cell) in t.header.iter().enumerate() {
                    jobs.push(Job {
                        index,
                        kind: Kind::TableHeader,
                        row: 0,
                        col,
                        text: cell.clone(),
                    });
                }
                for (row, cells) in t.rows.iter().enumerate() {
                    for (col, cell) in cells.iter().enumerate() {
                        jobs.push(Job {
                            index,
                            kind: Kind::TableCell,
                            row,
                            col,
                            text: cell.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // 3) Sırayla çevir (ücretsiz endpoint hız sınırına sahiptir).
    let total = jobs.len();
    let mut done = 0usize;
    for (n, job) in jobs.iter_mut().enumerate() {
        match tr.translate(&job.text).await {
            Ok(t) => {
                job.text = t;
                done += 1;
            }
            Err(e) => eprintln!("[çeviri] blok {} çevrilemedi: {e}", job.index),
        }
        if total >= 20 && (n + 1) % 10 == 0 {
            eprintln!("[çeviri] {}/{} blok...", n + 1, total);
        }
    }

    // 4) Çevrileri bloklara yaz.
    for job in &jobs {
        let block = match article.blocks.get_mut(job.index) {
            Some(b) => b,
            None => continue,
        };
        match (job.kind, block) {
            (Kind::Paragraph, Block::Paragraph(s)) => *s = job.text.clone(),
            (Kind::Heading, Block::Heading { text, .. }) => *text = job.text.clone(),
            (Kind::Caption, Block::Caption(s)) => *s = job.text.clone(),
            (Kind::Quote, Block::Quote(s)) => *s = job.text.clone(),
            (Kind::ListItem, Block::ListItem { text, .. }) => *text = job.text.clone(),
            (Kind::TableHeader, Block::Table(t)) => {
                if let Some(cell) = t.header.get_mut(job.col) {
                    *cell = job.text.clone();
                }
            }
            (Kind::TableCell, Block::Table(t)) => {
                if let Some(cell) = t.rows.get_mut(job.row).and_then(|r| r.get_mut(job.col)) {
                    *cell = job.text.clone();
                }
            }
            _ => {}
        }
    }

    eprintln!("[çeviri] {done}/{total} blok çevrildi");
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_and_restore_roundtrip() {
        let text = "Rust'ı `cargo build` ile derleyin; bkz https://doc.rust-lang.org ve <em>vurgu</em>.";
        let (masked, parts) = protect(text);
        // `cargo build`, URL, <em>, </em> → 4 korumalı parça.
        assert_eq!(parts.len(), 4);
        assert!(masked.contains(&marker(0)));
        assert!(!masked.contains("cargo build"));
        assert!(!masked.contains("https://"));
        assert!(!masked.contains("<em>"));
        // Kusursuz çeviri simülasyonu: maske geri açılınca birebir orijinal.
        assert_eq!(restore(&masked, &parts).as_deref(), Some(text));
    }

    #[test]
    fn protect_keeps_plain_text_untouched() {
        let (masked, parts) = protect("Sadece düz metin, korunan parça yok.");
        assert!(parts.is_empty());
        assert_eq!(masked, "Sadece düz metin, korunan parça yok.");
    }

    #[test]
    fn restore_missing_marker_signals_fallback() {
        let (_, parts) = protect("bir `kod` parçası");
        assert_eq!(restore("çeviri yer tutucuyu düşürdü", &parts), None);
    }

    #[test]
    fn translate_keeps_original_when_markers_dropped() {
        // Ağ yok: translate() Google yanıtı beklemeden marker-düşme yolunu
        // doğrulayamayız; bunun yerine protect/restore sözleşmesini test ederiz.
        let text = "bkz https://a.com/x ve `foo bar`";
        let (masked, parts) = protect(text);
        let translated = "bkz ve"; // Google'ın marker'ları düşürdüğü senaryo
        assert_eq!(
            restore(translated, &parts),
            None,
            "eksik marker fallback döndürmeli"
        );
        assert!(!masked.is_empty());
    }

    #[test]
    fn chunk_short_text_single_piece() {
        assert_eq!(chunk("kısa metin", 100), vec!["kısa metin".to_string()]);
    }

    #[test]
    fn chunk_splits_long_text_without_loss() {
        let words = std::iter::repeat_n("kelime", 1000)
            .collect::<Vec<_>>()
            .join(" ");
        let pieces = chunk(&words, 500);
        assert!(pieces.len() > 1, "uzun metin bölünmeli");
        assert!(pieces.iter().all(|p| p.chars().count() <= 500));
        assert_eq!(
            pieces.join(" ").split_whitespace().count(),
            1000,
            "hiçbir kelime kaybolmamalı"
        );
    }

    #[test]
    fn chunk_hard_splits_huge_single_word() {
        let word = "a".repeat(1200);
        let pieces = chunk(&word, 500);
        assert!(pieces.len() >= 3);
        assert_eq!(pieces.join(""), word, "karakterler birebir korunmalı");
    }
}
