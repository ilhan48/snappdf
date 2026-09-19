use scraper::{ElementRef, Html, Selector};

/// Çıkarılan makale.
#[derive(Debug, Clone)]
pub struct Article {
    pub title: String,
    /// Sıralı içerik blokları.
    pub blocks: Vec<Block>,
    /// Ana içerikte bulunan görsel URL'leri (sıra korunur, tekrarsız).
    pub images: Vec<String>,
}

/// Bir içerik bloğu.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading { level: u8, text: String },
    Paragraph(String),
    Code(String),
    Quote(String),
    ListItem(String),
    Image(String),
    Divider,
}

/// Çıkarım ayarları.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Bu kadar az metinli adaylar reddedilir.
    pub min_text_len: usize,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { min_text_len: 200 }
    }
}

/// Metin düğümünü temizler: fazla boşluklar tek boşluğa iner.
pub fn clean_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// <title> ya da ilk h1'den sayfa başlığını çıkarır.
pub fn extract_title(html: &Html) -> String {
    let title_sel = Selector::parse("title").expect("geçerli seçici");
    if let Some(t) = html.select(&title_sel).next() {
        let cleaned = clean_text(&t.text().collect::<String>());
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    let h1_sel = Selector::parse("h1").expect("geçerli seçici");
    if let Some(h) = html.select(&h1_sel).next() {
        let cleaned = clean_text(&h.text().collect::<String>());
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    "Başlıksız belge".to_string()
}

/// Aday makale konteynerlerini skorlar ve en iyisini seçer (readability benzeri).
///
/// Liste geniş: yaygın blog motorları (WordPress, Ghost, Medium, Substack,
/// Hugo, Jekyll, Vox/Chorus) ve docs siteleri (Docusaurus, MkDocs, GitBook,
/// Read the Docs, VitePress) hedeflenir. Skorlama yanlış adayı eler:
/// dar kapsayıcılar (sidebar, nav) düşük puan alır.
fn best_container<'a>(html: &'a Html) -> Option<ElementRef<'a>> {
    let candidates = [
        // genel semantik
        "article",
        "main",
        "[role=main]",
        "#content",
        "#main-content",
        "#main",
        // blog motorları (WordPress, Ghost, Hugo, Jekyll, Medium, Vox...)
        ".post-content",
        ".entry-content",
        ".article-content",
        ".article-body",
        ".post-body",
        ".post__content",
        ".gh-content",
        ".blog-post-content",
        ".entry-body",
        ".story-body",
        ".c-entry-content",
        ".articleBody",
        ".e-content", // microformats2 (h-entry)
        // Substack
        ".available-content",
        ".body.markup",
        ".markup",
        // docs siteleri
        ".markdown-body",      // GitHub
        ".md-content",         // MkDocs Material
        ".theme-doc-markdown", // Docusaurus
        ".docs-content",
        ".doc-content",
        ".documentation",
        ".rst-content",    // Read the Docs (Sphinx)
        ".wy-nav-content", // Read the Docs teması
        ".vp-doc",         // VitePress
        ".content",
        ".prose", // Tailwind tipografi
    ];
    let mut best: Option<(ElementRef<'a>, usize)> = None;
    for sel in candidates {
        let Ok(sel) = Selector::parse(sel) else {
            continue;
        };
        for el in html.select(&sel) {
            let score = text_score(el);
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((el, score));
            }
        }
    }
    best.map(|(el, _)| el)
}

fn text_score(el: ElementRef) -> usize {
    let mut len = 0usize;
    let mut p_count = 0usize;
    for descendant in el.descendants() {
        let node = descendant.value();
        if let Some(t) = node.as_text() {
            len += clean_text(t).len();
        }
        if let Some(e) = node.as_element() {
            if e.name() == "p" {
                p_count += 1;
            }
        }
    }
    len + p_count * 50
}

/// Basit "metin ağırlıklı" eleman mı? (görsel/boş yapı değil)
#[cfg(test)]
fn is_contentful(el: &ElementRef) -> bool {
    let text = clean_text(&el.text().collect::<String>());
    if text.len() < 3 && el.select(&Selector::parse("img").unwrap()).next().is_none() {
        return false;
    }
    true
}

/// Tek bir blok elemanını Block'a çevirir.
fn block_from(el: ElementRef, images: &mut Vec<String>, base: &str) -> Option<Block> {
    let name = el.value().name();
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name.as_bytes()[1] - b'0';
            let text = clean_text(&el.text().collect::<String>());
            if text.is_empty() {
                None
            } else {
                Some(Block::Heading { level, text })
            }
        }
        "p" => {
            let text = clean_text(&el.text().collect::<String>());
            if text.is_empty() {
                // Paragraf içinde görsel olabilir.
                img_in(el, images, base).map(Block::Image)
            } else {
                Some(Block::Paragraph(text))
            }
        }
        "pre" => {
            let text = el.text().collect::<String>();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(Block::Code(trimmed.to_string()))
            }
        }
        "blockquote" => {
            let text = clean_text(&el.text().collect::<String>());
            if text.is_empty() {
                None
            } else {
                Some(Block::Quote(text))
            }
        }
        "li" => {
            let text = clean_text(&el.text().collect::<String>());
            if text.is_empty() {
                None
            } else {
                Some(Block::ListItem(text))
            }
        }
        "img" => img_in(el, images, base).map(Block::Image),
        "hr" => Some(Block::Divider),
        _ => None,
    }
}

/// Elemandaki ilk anlamlı görsel kaynağını toplar.
/// Elemanın kendisi <img> olabilir ya da içinde bir <img> bulunabilir.
fn img_in(el: ElementRef, images: &mut Vec<String>, base: &str) -> Option<String> {
    let img_sel = Selector::parse("img").expect("geçerli seçici");
    let self_img = if el.value().name() == "img" {
        Some(el)
    } else {
        None
    };
    let img = self_img.or_else(|| el.select(&img_sel).next())?;
    let src = img
        .value()
        .attr("src")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            // lazy-load: data-src yedekleri
            img.value()
                .attr("data-src")
                .or_else(|| img.value().attr("data-original"))
                .map(str::to_string)
        })?;
    if src.starts_with("data:") {
        return None;
    }
    let abs = crate::fetch::absolute_url(base, &src)?;
    if !images.contains(&abs) {
        images.push(abs.clone());
    }
    Some(abs)
}

/// HTML'den makale içeriğini çıkarır. `base` görsel URL'lerinin çözümü için kullanılır.
pub fn extract(
    html_text: &str,
    base: &str,
    opts: &ExtractOptions,
) -> Result<Article, ExtractError> {
    let html = Html::parse_document(html_text);
    let title = extract_title(&html);

    let container = best_container(&html).ok_or(ExtractError::NoContent)?;
    let total = text_score(container);
    if total < opts.min_text_len {
        return Err(ExtractError::TooThin {
            score: total,
            min: opts.min_text_len,
        });
    }

    let mut blocks = Vec::new();
    let mut images = Vec::new();
    let skip = [
        "nav", "aside", "footer", "script", "style", "noscript", "form", "button", "svg", "iframe",
        "header",
    ];

    for child in container.descendants() {
        let Some(el) = ElementRef::wrap(child) else {
            continue;
        };
        let name = el.value().name();
        if skip.contains(&name) {
            continue;
        }
        // İç içe bloklarda tekrar: pre içinde p, li içinde p gibi. Derinlik
        // çakışmasını önlemek için yalnızca doğrudan blok adaylarını al,
        // çocuğu olan blokları atla (blockquote/li/pre/kendi işler).
        if matches!(name, "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            && ancestors_are_blocks(&el)
        {
            continue;
        }
        if let Some(b) = block_from(el, &mut images, base) {
            blocks.push(b);
        }
    }

    if blocks.is_empty() {
        return Err(ExtractError::NoBlocks);
    }

    Ok(Article {
        title,
        blocks,
        images,
    })
}

/// Elemanın atası p/li/pre/blockquote mi? (iç içe paragraf tekrarını önler)
fn ancestors_are_blocks(el: &ElementRef) -> bool {
    let mut cur = el.parent();
    while let Some(node) = cur {
        if let Some(pe) = ElementRef::wrap(node) {
            let n = pe.value().name();
            if matches!(n, "p" | "li" | "pre" | "blockquote") {
                return true;
            }
            if n == "body" || n == "html" {
                return false;
            }
        }
        cur = node.parent();
    }
    false
}

/// Çıkarım hataları.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("makale gövdesi bulunamadı")]
    NoContent,
    #[error("içerik çok ince (skor {score} < {min})")]
    TooThin { score: usize, min: usize },
    #[error("içerik bloğu çıkarılamadı")]
    NoBlocks,
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    <html>
    <head><title>Deneme Başlık</title></head>
    <body>
      <nav>Menü gezinme hiç kullanılmayacak</nav>
      <article>
        <h1>Deneme Başlık</h1>
        <p>Bu ilk paragraf ve içinde Türkçe karakterler var: ğüşıöçİĞÜŞÖÇ.</p>
        <p>İkinci paragraf biraz daha uzun olsun ki metin skorumuz iyi olsun
           ve çıkarım testinde gerçekten makale gövdesi seçilsin. Lorem ipsum
           dolor sit amet consectetur adipiscing elit sed do eiusmod tempor.</p>
        <pre><code>fn main() { println!("merhaba"); }</code></pre>
        <blockquote>Önemli bir alıntı cümlesi burada.</blockquote>
        <ul><li>Liste ögesi bir</li><li>Liste ögesi iki</li></ul>
        <img src="/img/macera.png" alt="macera">
        <hr>
        <h2>Alt başlık</h2>
        <p>Kapanış paragrafı.</p>
      </article>
      <aside>Reklam kenar çubuğu</aside>
    </body>
    </html>"#;

    fn opts() -> ExtractOptions {
        ExtractOptions { min_text_len: 50 }
    }

    #[test]
    fn clean_text_collapses_whitespace() {
        assert_eq!(clean_text("  a \n\t b  c "), "a b c");
        assert_eq!(clean_text(""), "");
        assert_eq!(clean_text("tek"), "tek");
    }

    #[test]
    fn extract_title_prefers_title_tag() {
        let html = Html::parse_document(SAMPLE);
        assert_eq!(extract_title(&html), "Deneme Başlık");
    }

    #[test]
    fn extract_title_falls_back_to_h1_then_default() {
        let html = Html::parse_document("<html><body><h1>H1 Başlık</h1></body></html>");
        assert_eq!(extract_title(&html), "H1 Başlık");
        let empty = Html::parse_document("<html><body><p>x</p></body></html>");
        assert_eq!(extract_title(&empty), "Başlıksız belge");
    }

    #[test]
    fn extract_full_pipeline_blocks_and_images() {
        let art = extract(SAMPLE, "https://ornek.com/yazi", &opts()).unwrap();
        assert_eq!(art.title, "Deneme Başlık");
        assert!(matches!(&art.blocks[0], Block::Heading { level: 1, .. }));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("ğüşıöç"))));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Code(c) if c.contains("println"))));
        assert!(art.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
        assert!(art.blocks.iter().any(|b| matches!(b, Block::ListItem(_))));
        assert!(art.blocks.iter().any(|b| matches!(b, Block::Divider)));
        assert_eq!(
            art.images,
            vec!["https://ornek.com/img/macera.png".to_string()]
        );
    }

    #[test]
    fn extract_rejects_thin_content() {
        // "kısa" (4 harf) + 1 paragraf * 50 = 54 puan; eşiği aşmasın.
        let html = "<html><body><article><p>kısa</p></article></body></html>";
        let strict = ExtractOptions { min_text_len: 100 };
        let err = extract(html, "https://a.com", &strict).unwrap_err();
        assert!(matches!(err, ExtractError::TooThin { .. }));
        let ExtractError::TooThin { score, min } = err else {
            unreachable!()
        };
        assert!(score < min);
    }

    #[test]
    fn extract_rejects_when_no_container() {
        let html = "<html><body><div>sekmeler menü footer gibi şeyler</div></body></html>";
        let err = extract(html, "https://a.com", &opts()).unwrap_err();
        assert!(matches!(err, ExtractError::NoContent));
    }

    /// Site platformu senaryoları: her platformun tipik DOM iskeleti,
    /// gerçek dünyada görüldüğü şekliyle sadeleştirilmiş haliyle.
    mod platforms {
        use super::*;

        fn extract_first_paragraph(html: &str) -> String {
            let art = extract(
                html,
                "https://site.com/yazi",
                &ExtractOptions { min_text_len: 50 },
            )
            .expect("çıkarım başarılı olmalı");
            let title = art.title;
            assert!(!title.is_empty());
            art.blocks
                .iter()
                .find_map(|b| match b {
                    Block::Paragraph(t) => Some(t.clone()),
                    _ => None,
                })
                .expect("en az bir paragraf olmalı")
        }

        #[test]
        fn wordpress_classic_entry_content() {
            let html = r#"
            <html><head><title>WP Yazı</title></head><body>
              <div class="site"><nav>menü</nav>
                <article class="post"><div class="entry-content">
                  <p>WordPress klasik editör çıktısı gövdesi.</p>
                </div></article>
              </div></body></html>"#;
            assert!(extract_first_paragraph(html).contains("WordPress"));
        }

        #[test]
        fn ghost_gh_content() {
            let html = r#"
            <html><head><title>Ghost Yazı</title></head><body>
              <article class="post"><section class="gh-content">
                <p>Ghost resmi teması içerik bölümü.</p>
              </section></article>
            </body></html>"#;
            assert!(extract_first_paragraph(html).contains("Ghost"));
        }

        #[test]
        fn medium_and_substack_markup() {
            let medium = r#"
            <html><head><title>Medium Yazı</title></head><body>
              <article><section class="articleBody">
                <p>Medium gövde paragrafı.</p>
              </section></article>
            </body></html>"#;
            assert!(extract_first_paragraph(medium).contains("Medium"));

            let substack = r#"
            <html><head><title>Substack Yazı</title></head><body>
              <div class="available-content"><div class="body markup">
                <p>Substack bülten gövdesi.</p>
              </div></div>
            </body></html>"#;
            assert!(extract_first_paragraph(substack).contains("Substack"));
        }

        #[test]
        fn docusaurus_and_mkdocs() {
            let docusaurus = r#"
            <html><head><title>Docusaurus</title></head><body>
              <article><div class="theme-doc-markdown">
                <p>Docusaurus doküman sayfası içeriği burada.</p>
              </div></article>
            </body></html>"#;
            assert!(extract_first_paragraph(docusaurus).contains("Docusaurus"));

            let mkdocs = r#"
            <html><head><title>MkDocs</title></head><body>
              <main><div class="md-content">
                <p>MkDocs Material içerik alanı.</p>
              </div></main>
            </body></html>"#;
            assert!(extract_first_paragraph(mkdocs).contains("MkDocs"));
        }

        #[test]
        fn read_the_docs_rst_content() {
            let html = r#"
            <html><head><title>Sphinx</title></head><body>
              <div class="wy-nav-content"><div class="rst-content">
                <p>Sphinx/Read the Docs gövdesi.</p>
              </div></div>
            </body></html>"#;
            assert!(extract_first_paragraph(html).contains("Sphinx"));
        }

        #[test]
        fn vitepress_and_hugo() {
            let vitepress = r#"
            <html><head><title>VitePress</title></head><body>
              <main><div class="vp-doc">
                <p>VitePress doküman paragrafı.</p>
              </div></main>
            </body></html>"#;
            assert!(extract_first_paragraph(vitepress).contains("VitePress"));

            let hugo = r#"
            <html><head><title>Hugo</title></head><body>
              <main><article class="post"><div class="post__content">
                <p>Hugo tema içerik alanı.</p>
              </div></article></main>
            </body></html>"#;
            assert!(extract_first_paragraph(hugo).contains("Hugo"));
        }

        #[test]
        fn higher_score_wins_over_sidebar() {
            // .content hem sidebar hem makaleyi kapsıyor; makale metni uzun
            // olduğu için skorlama doğru bloğu seçmeli.
            let html = r#"
            <html><head><title>Skor</title></head><body>
              <div class="content">
                <aside class="sidebar">kısa link listesi</aside>
                <div><p>{}</p></div>
              </div>
            </body></html>"#;
            let long = "Uzun makale paragrafı ".repeat(20);
            let html = &html.replace("{}", &long);
            let art = extract(html, "https://s.com", &ExtractOptions { min_text_len: 50 }).unwrap();
            assert!(art
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Paragraph(t) if t.contains("Uzun makale"))));
        }
    }

    #[test]
    fn extract_rejects_when_no_blocks() {
        // Konteyner var ama blok üretilemiyor. Script metni skora katılır
        // (8 puan); eşiği altında tutup NoBlocks'a düşmesini sağlıyoruz.
        let html = r#"<html><body><main><script>var x=1;</script></main></body></html>"#;
        let lax = ExtractOptions { min_text_len: 5 };
        let err = extract(html, "https://a.com", &lax).unwrap_err();
        assert!(matches!(err, ExtractError::NoBlocks));
    }

    #[test]
    fn extract_uses_data_src_for_lazy_images() {
        let html = r#"
        <html><body><article>
          <p>{}</p>
        </article></body></html>"#;
        let _ = html; // (kapsam: img_in doğrudan test ediliyor)
        let frag = Html::parse_fragment(r#"<img data-src="/lazy.png">"#);
        let el = frag.root_element();
        let mut images = Vec::new();
        let got = img_in(el, &mut images, "https://b.com/post");
        assert_eq!(got.as_deref(), Some("https://b.com/lazy.png"));
        assert_eq!(images, vec!["https://b.com/lazy.png"]);
    }

    #[test]
    fn img_in_skips_data_urls_and_duplicates() {
        let frag = Html::parse_fragment(r#"<img src="data:image/png;base64,AAA">"#);
        let mut images = Vec::new();
        assert!(img_in(frag.root_element(), &mut images, "https://b.com").is_none());

        let frag = Html::parse_fragment(r#"<img src="https://b.com/a.png">"#);
        let mut images = vec!["https://b.com/a.png".to_string()];
        let got = img_in(frag.root_element(), &mut images, "https://b.com");
        assert_eq!(got.as_deref(), Some("https://b.com/a.png"));
        assert_eq!(images.len(), 1, "tekrar eklenmemeli");
    }

    #[test]
    fn block_from_handles_all_variants() {
        let frag = Html::parse_fragment(
            r#"<div><h3>Başlık</h3><p>Paragraf</p><pre>kod</pre><blockquote>alıntı</blockquote><li>madde</li><hr></div>"#,
        );
        let mut images = Vec::new();
        let mut found = Vec::new();
        for child in frag.root_element().descendants() {
            if let Some(el) = ElementRef::wrap(child) {
                if let Some(b) = block_from(el, &mut images, "https://c.com") {
                    found.push(b);
                }
            }
        }
        assert!(found.contains(&Block::Heading {
            level: 3,
            text: "Başlık".into()
        }));
        assert!(found.contains(&Block::Paragraph("Paragraf".into())));
        assert!(found.contains(&Block::Code("kod".into())));
        assert!(found.contains(&Block::Quote("alıntı".into())));
        assert!(found.contains(&Block::ListItem("madde".into())));
        assert!(found.contains(&Block::Divider));
    }

    #[test]
    fn empty_text_blocks_are_dropped() {
        let frag = Html::parse_fragment("<p>   </p><h2></h2><pre>  </pre>");
        let mut images = Vec::new();
        let mut count = 0;
        for child in frag.root_element().descendants() {
            if let Some(el) = ElementRef::wrap(child) {
                if block_from(el, &mut images, "https://c.com").is_some() {
                    count += 1;
                }
            }
        }
        assert_eq!(count, 0);
    }

    #[test]
    fn paragraph_with_image_only_becomes_image_block() {
        let frag = Html::parse_fragment(r#"<p><img src="https://x.com/i.png"></p>"#);
        let mut images = Vec::new();
        let mut got = None;
        for child in frag.root_element().descendants() {
            if let Some(el) = ElementRef::wrap(child) {
                if el.value().name() == "p" {
                    if let Some(b) = block_from(el, &mut images, "https://x.com") {
                        got = Some(b);
                    }
                }
            }
        }
        assert_eq!(got, Some(Block::Image("https://x.com/i.png".into())));
    }

    #[test]
    fn is_contentful_distinguishes_empty_and_image_elements() {
        let frag = Html::parse_fragment(
            "<div><span>   </span></div><div><img src='/i.png'></div><div>metin</div>",
        );
        let divs: Vec<ElementRef> = frag.select(&Selector::parse("div").unwrap()).collect();
        assert!(!is_contentful(&divs[0]));
        assert!(is_contentful(&divs[1]));
        assert!(is_contentful(&divs[2]));
    }

    #[test]
    fn nested_paragraphs_are_not_duplicated() {
        // blockquote içindeki p, quote bloğu zaten kapsadığı için ayrı paragraf olmamalı.
        let html = r#"
        <html><body><article>
          <p>Ana paragraf burada, yeterince uzun bir metin ile yazılmış durumdadır ki
          makale gövdesi seçilsin ve çıkarım başarılı sayılsın. Ek cümleler de var.</p>
          <blockquote><p>Alıntı içi paragraf.</p></blockquote>
        </article></body></html>"#;
        let art = extract(html, "https://a.com", &opts()).unwrap();
        let para_count = art
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count();
        assert_eq!(para_count, 1, "blockquote içindeki p tekrarlanmamalı");
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Quote(t) if t.contains("Alıntı"))));
    }

    #[test]
    fn extract_error_messages_are_human_readable() {
        assert_eq!(
            ExtractError::NoContent.to_string(),
            "makale gövdesi bulunamadı"
        );
        assert_eq!(
            ExtractError::NoBlocks.to_string(),
            "içerik bloğu çıkarılamadı"
        );
        let e = ExtractError::TooThin { score: 3, min: 10 };
        assert_eq!(e.to_string(), "içerik çok ince (skor 3 < 10)");
    }

    #[test]
    fn default_options_values() {
        let o = ExtractOptions::default();
        assert_eq!(o.min_text_len, 200);
    }

    #[test]
    fn text_score_rewards_paragraphs() {
        let doc1 = Html::parse_document("<div><p>abcdef</p><p>ghijkl</p></div>");
        let doc2 = Html::parse_document("<div><span>abcdefghijkl</span></div>");
        let s1 = text_score(doc1.root_element());
        let s2 = text_score(doc2.root_element());
        assert!(s1 > s2, "p içeren skor daha yüksek olmalı: {s1} vs {s2}");
    }
}
