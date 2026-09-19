use ego_tree::NodeRef;
use scraper::node::Node;
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

/// Bir tablo. Hücre metinleri satır sonu (`\n`) içerebilir.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    /// Başlık satırı (thead/th yoksa boş).
    pub header: Vec<String>,
    /// Gövde satırları. Her satır `columns` uzunluğundadır.
    pub rows: Vec<Vec<String>>,
    /// Sütun sayısı.
    pub columns: usize,
}

impl Table {
    /// Belge kurulurken sütun ağırlıklarını hesaplamak için toplam metin uzunluğu.
    pub fn is_empty(&self) -> bool {
        self.header.is_empty() && self.rows.is_empty()
    }
}

/// Bir içerik bloğu.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    /// Görsel altı açıklaması (figcaption).
    Caption(String),
    Code(String),
    Quote(String),
    /// Liste ögesi. `depth` iç içe liste derinliği (0 = en dış).
    ListItem {
        text: String,
        ordered: bool,
        depth: u8,
    },
    Table(Table),
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

/// Asla içeriğe dahil edilmeyen etiketler (tüm alt ağaçlarıyla atlanır).
const SKIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "nav", "aside", "footer", "header", "form",
    "button", "select", "textarea", "label", "input", "svg", "iframe", "menu", "dialog", "audio",
    "video", "canvas", "object", "embed", "map", "area",
];

/// Şablon/kabuk (boilerplate) işaretleri: `class` ya da `id` içinde geçerse
/// alt ağaç tamamen atlanır. Amaç, makale gövdesine yan içerik sızmasını önlemek.
const BOILERPLATE_MARKERS: &[&str] = &[
    "related",
    "rel-post",
    "sharedaddy",
    "addthis",
    "addtoany",
    "social-share",
    "share-links",
    "share-buttons",
    "sidebar",
    "widget",
    "comment",
    "breadcrumb",
    "pagination",
    "pager",
    "newsletter",
    "subscribe",
    "advert",
    "sponsor",
    "cookie",
    "banner",
    "popup",
    "modal",
    "disqus",
    "recommend",
    "post-navigation",
    "nav-links",
    "screen-reader",
    "skip-link",
    "visually-hidden",
    "sr-only",
    "table-of-contents",
    "author-box",
    "previous-post",
    "next-post",
];

/// Bu eleman ve alt ağacı tamamen atlanmalı mı?
fn skip_element(el: &ElementRef) -> bool {
    let v = el.value();
    let name = v.name();
    if SKIP_TAGS.contains(&name) {
        return true;
    }
    if v.attr("hidden").is_some() || v.attr("aria-hidden") == Some("true") {
        return true;
    }
    // inline display:none — tema gizli meta satırlarını böyle işaretler
    if let Some(style) = v.attr("style") {
        let compact: String = style.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("display:none") || compact.contains("visibility:hidden") {
            return true;
        }
    }
    if let Some(role) = v.attr("role") {
        if matches!(
            role,
            "navigation" | "banner" | "search" | "complementary" | "contentinfo" | "form"
        ) {
            return true;
        }
    }
    let mut hay = String::new();
    if let Some(class) = v.attr("class") {
        hay.push_str(class);
    }
    if let Some(id) = v.attr("id") {
        hay.push(' ');
        hay.push_str(id);
    }
    !hay.is_empty() && has_boilerplate_marker(&hay)
}

/// `class`/`id` metnini karşılaştırmaya hazırlar: harf/rakam dışı her karakter
/// ayraç sayılır ve boşluğa dönüşür, başa-sona da ayraç eklenir.
fn normalize_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(' ');
        }
    }
    out.push(' ');
    out
}

/// Kabuk (boilerplate) işareti arar.
///
/// Önemli: düz `contains` kullanmak yanlış eşleşmelere yol açıyordu — örneğin
/// `tag-multimodalmessage` sınıfı "modal" işaretine takılıp makalenin tamamını
/// eliyordu. Bu yüzden tek kelimelik işaretler **kelime başında** aranır
/// ("advert" ~ "advertisement" eşleşir, "modal" ~ "multimodal" eşleşmez),
/// çok kelimeli işaretler ise bitişik ifade olarak aranır.
fn has_boilerplate_marker(hay: &str) -> bool {
    let norm = normalize_tokens(hay);
    let tokens: Vec<&str> = norm.split_whitespace().collect();

    // WordPress taksonomi sınıfları (`tag-modal-dialog`, `category-...`) etiket
    // slug'ıdır, arayüz kabı değildir. Bunları eşleştirmeden çıkarıyoruz; aksi
    // halde modal pencereler üzerine yazılmış bir makale "modal" işaretine
    // takılıp elenir.
    let meaningful: Vec<&str> = tokens
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            if *i == 0 {
                return true;
            }
            !TAXONOMY_PREFIXES.contains(&tokens[i - 1])
        })
        .map(|(_, t)| *t)
        .collect();

    BOILERPLATE_MARKERS.iter().any(|marker| {
        let m = normalize_tokens(marker);
        let phrase = m.trim();
        if phrase.contains(' ') {
            // Çok kelimeli işaret: bitişik ifade olarak aranır.
            norm.contains(&m)
        } else {
            meaningful.iter().any(|t| t.starts_with(phrase))
        }
    })
}

/// `class`/`id` içinde taksonomi slug'ı başlatan önekler (WordPress vb.).
const TAXONOMY_PREFIXES: &[&str] = &["tag", "category", "cat"];

/// <title> ve ilk h1'den sayfa başlığını çıkarır.
pub fn extract_title(html: &Html) -> String {
    let raw = title_tag_text(html)
        .or_else(|| first_h1_text(html))
        .unwrap_or_else(|| "Başlıksız belge".to_string());
    clean_title(&raw)
}

fn title_tag_text(html: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let t = html.select(&sel).next()?;
    let cleaned = clean_text(&t.text().collect::<String>());
    (!cleaned.is_empty()).then_some(cleaned)
}

fn first_h1_text(html: &Html) -> Option<String> {
    let sel = Selector::parse("h1").ok()?;
    let h = html.select(&sel).next()?;
    let cleaned = clean_text(&h.text().collect::<String>());
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Site adı ekini temizler: "Yazı Başlığı | Site Adı" -> "Yazı Başlığı".
pub fn clean_title(raw: &str) -> String {
    let t = clean_text(raw);
    for sep in [" | ", " :: ", " » ", " — ", " – "] {
        if let Some((head, _)) = t.split_once(sep) {
            let head = head.trim();
            if head.chars().count() >= 15 {
                return head.to_string();
            }
        }
    }
    t
}

/// Aday makale konteynerlerini skorlar ve en iyisini seçer.
///
/// İki aşamalı: önce bilinen platform seçicileri denenir (WordPress, Ghost,
/// Medium, Substack, Hugo, Docusaurus, MkDocs, GitBook, Read the Docs,
/// VitePress...). Hiçbiri yoksa yapısal bir geri dönüş çalışır (`article`,
/// `main`, `section`, `div`) — böylece şablonu tanımadığımız siteler ve
/// portfolyo/açılış sayfaları da PDF'e dönüştürülebilir.
///
/// Skor, boilerplate alt ağaçları atlanarak hesaplanır; eşitlikte daha küçük
/// (daha özgül) konteyner kazanır. Bu, `<main class="content">` gibi geniş
/// sarmalayıcıların makalenin önüne geçmesini engeller.
fn best_container<'a>(html: &'a Html, min_text_len: usize) -> (Option<ElementRef<'a>>, usize) {
    let candidates = [
        // genel semantik
        "article",
        "main",
        "[role=main]",
        "#content",
        "#main-content",
        "#main",
        // blog motorları (WordPress, Ghost, Hugo, Jekyll, Medium, Vox, Hueman...)
        ".post-content",
        ".entry-content",
        ".entry-inner",
        ".entry-body",
        ".entry",
        ".post-inner",
        ".article-content",
        ".article-body",
        ".article__body",
        ".post-body",
        ".post__content",
        ".post-content-inner",
        ".single-content",
        ".gh-content",
        ".blog-post-content",
        ".story-body",
        ".c-entry-content",
        ".articleBody",
        ".td-post-content",
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

    // (skor, alt eleman sayısı, eleman)
    let mut found: Vec<(usize, usize, ElementRef<'a>)> = Vec::new();

    collect_candidates(html, &candidates, &mut found);
    let mut best_score = max_score(&found);
    if best_score < min_text_len {
        // Yapısal geri dönüş: bilinen seçici yeterli değilse en yoğun metin kabını
        // ara. Şablonunu tanımadığımız siteler ve açılış sayfaları böylece çalışır.
        collect_candidates(html, &["article", "main", "section", "div"], &mut found);
        best_score = max_score(&found);
    }
    if best_score < min_text_len {
        // Son çare: gövdenin tamamı. Nav/footer/aside budandığı için sade
        // sayfalarda doğru sonuç verir.
        collect_candidates(html, &["body"], &mut found);
        best_score = max_score(&found);
    }
    if best_score == 0 {
        return (None, 0);
    }

    // En yüksek skorlu aday, genelde makaleyi + yan içeriği kapsayan geniş bir
    // sarmalayıcıdır. Ona yakın (skorun %90'ı ve üzeri) adaylar arasından en
    // küçüğünü seçerek asıl içerik kabını buluyoruz.
    let floor = best_score * 9 / 10;
    let chosen = found
        .into_iter()
        .filter(|(score, _, _)| *score >= floor && *score >= min_text_len)
        .min_by_key(|(_, size, _)| *size)
        .map(|(_, _, el)| el);
    (chosen, best_score)
}

fn max_score<'a>(found: &[(usize, usize, ElementRef<'a>)]) -> usize {
    found.iter().map(|(s, _, _)| *s).max().unwrap_or(0)
}

fn collect_candidates<'a>(
    html: &'a Html,
    selectors: &[&str],
    out: &mut Vec<(usize, usize, ElementRef<'a>)>,
) {
    for sel in selectors {
        let Ok(sel) = Selector::parse(sel) else {
            continue;
        };
        for el in html.select(&sel) {
            out.push((text_score(el), el.descendants().count(), el));
        }
    }
}

/// Metin skoru: paragraf metni uzunluğu + paragraf başına prim.
/// Boilerplate alt ağaçlar (reklam, yan içerik, yorumlar) sayılmaz.
fn text_score(el: ElementRef) -> usize {
    score_node(*el)
}

fn score_node(node: NodeRef<Node>) -> usize {
    if let Some(el) = ElementRef::wrap(node) {
        if skip_element(&el) {
            return 0;
        }
        let mut score = 0;
        if el.value().name() == "p" {
            score += 50;
        }
        for child in node.children() {
            score += score_node(child);
        }
        return score;
    }
    if let Some(t) = node.value().as_text() {
        return clean_text(t).len();
    }
    0
}

/// Bir düğüm ağacındaki metni toplar. `skip` true dönen elemanların alt ağacı
/// tamamen atlanır ve <br> satır sonuna dönüşür.
fn collect_text(node: NodeRef<Node>, out: &mut String, skip: &dyn Fn(&ElementRef) -> bool) {
    if let Some(el) = ElementRef::wrap(node) {
        if skip_element(&el) || skip(&el) {
            return;
        }
        if el.value().name() == "br" {
            out.push('\n');
            return;
        }
        for child in node.children() {
            collect_text(child, out, skip);
        }
        return;
    }
    if let Some(t) = node.value().as_text() {
        out.push_str(t);
        out.push(' ');
    }
}

/// Hiçbir şeyi atlamayan yardımcı.
fn never(_el: &ElementRef) -> bool {
    false
}

/// Elemanın kendi metni (alt ağaç atlamaları hariç), satır sonları korunarak.
fn own_text(el: ElementRef) -> String {
    let mut s = String::new();
    collect_text(*el, &mut s, &never);
    s
}

/// Hücre/öge metni: satır sonları temizlenmiş, boşlukları sadeleştirilmiş.
fn flat_text(raw: &str) -> String {
    raw.lines()
        .map(clean_text)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bir hücrenin metnini satırlara ayırır: <br>, <li>, <p> sınırlarında bölünür.
/// Liste ögeleri "• " ile işaretlenir.
fn cell_text(el: ElementRef) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    collect_lines(*el, &mut lines, &mut cur);
    flush(&mut cur, &mut lines);
    lines.join("\n")
}

fn flush(cur: &mut String, lines: &mut Vec<String>) {
    let t = clean_text(cur);
    if !t.is_empty() {
        lines.push(t);
    }
    cur.clear();
}

fn collect_lines(node: NodeRef<Node>, lines: &mut Vec<String>, cur: &mut String) {
    if let Some(el) = ElementRef::wrap(node) {
        if skip_element(&el) {
            return;
        }
        let name = el.value().name();
        if name == "br" {
            flush(cur, lines);
            return;
        }
        if name == "li" {
            flush(cur, lines);
            let before = lines.len();
            for child in node.children() {
                collect_lines(child, lines, cur);
            }
            flush(cur, lines);
            if lines.len() > before {
                lines[before].insert_str(0, "• ");
            }
            return;
        }
        let is_block = matches!(
            name,
            "p" | "div"
                | "tr"
                | "table"
                | "ul"
                | "ol"
                | "blockquote"
                | "section"
                | "article"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
        );
        for child in node.children() {
            collect_lines(child, lines, cur);
        }
        if is_block {
            flush(cur, lines);
        }
        return;
    }
    if let Some(t) = node.value().as_text() {
        cur.push_str(t);
        cur.push(' ');
    }
}

/// Elemandaki ilk anlamlı görsel kaynağını toplar.
/// Elemanın kendisi <img> olabilir ya da içinde bir <img> bulunabilir.
fn img_in(el: ElementRef, images: &mut Vec<String>, base: &str) -> Option<String> {
    let img_sel = Selector::parse("img").expect("geçerli seçici");
    let img = if el.value().name() == "img" {
        el
    } else {
        el.select(&img_sel).next()?
    };
    let v = img.value();
    let raw = [
        "src",
        "data-src",
        "data-lazy-src",
        "data-original",
        "data-url",
    ]
    .iter()
    .find_map(|attr| v.attr(attr).map(str::trim).filter(|s| !s.is_empty()))
    .map(str::to_string)
    .or_else(|| srcset_first_url(v.attr("srcset").or_else(|| v.attr("data-srcset"))))?;
    if raw.starts_with("data:") {
        return None;
    }
    let abs = crate::fetch::absolute_url(base, &raw)?;
    if !images.contains(&abs) {
        images.push(abs.clone());
    }
    Some(abs)
}

/// srcset'ten ilk URL'yi seçer: "a.png 1x, b.png 2x" -> "a.png".
fn srcset_first_url(srcset: Option<&str>) -> Option<String> {
    let first = srcset?.split(',').next()?.split_whitespace().next()?;
    (!first.is_empty()).then(|| first.to_string())
}

/// Tabloyu çıkarır. İç içe tablolar yok sayılır.
fn table_from(el: ElementRef) -> Option<Table> {
    let tr_sel = Selector::parse("tr").ok()?;
    let mut all: Vec<(Vec<String>, bool)> = Vec::new();

    for tr in el.select(&tr_sel) {
        if within_nested_table(tr, el) {
            continue;
        }
        let mut cells: Vec<String> = Vec::new();
        let mut header = in_thead(tr, el);
        for child in tr.children() {
            let Some(cell) = ElementRef::wrap(child) else {
                continue;
            };
            match cell.value().name() {
                "td" => cells.push(cell_text(cell)),
                "th" => {
                    header = true;
                    cells.push(cell_text(cell));
                }
                _ => {}
            }
        }
        if !cells.is_empty() {
            all.push((cells, header));
        }
    }

    if all.is_empty() {
        return None;
    }

    let columns = all.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    let normalize = |mut cells: Vec<String>| -> Vec<String> {
        cells.resize(columns, String::new());
        cells
    };

    let first_is_header = all[0].1;
    let (header, body) = if first_is_header {
        (normalize(all[0].0.clone()), all[1..].to_vec())
    } else {
        (Vec::new(), all.clone())
    };

    let rows: Vec<Vec<String>> = body
        .into_iter()
        .map(|(c, _)| normalize(c))
        .filter(|r| r.iter().any(|c| !c.is_empty()))
        .collect();

    if header.is_empty() && rows.is_empty() {
        return None;
    }
    Some(Table {
        header,
        rows,
        columns,
    })
}

/// Düzen (layout) tablosu mu?
///
/// Eski tip siteler ve statik bloglar sayfa iskeletini `<table>` ile kurar
/// (yüzlerce satır, başlık hücresi yok). Bunları ızgara olarak basmak okunaksız
/// olur; içerik sıradan akış gibi gezilmelidir.
fn is_layout_table(el: ElementRef, table: &Table) -> bool {
    // Gerçek veri tablosunun başlığı olur.
    if !table.header.is_empty() {
        return false;
    }
    if table.rows.len() > MAX_CONTENT_TABLE_ROWS {
        return true;
    }
    // İçinde yine tablo barındıran küçük bir tablo, iç içe düzen tablosudur.
    if table.rows.len() > 8 {
        if let Ok(sel) = Selector::parse("table table") {
            return el.select(&sel).next().is_some();
        }
    }
    false
}

/// Başlıksız bir tablonun "veri tablosu" sayılabileceği en fazla satır sayısı.
const MAX_CONTENT_TABLE_ROWS: usize = 30;

/// `tr`, `table_el`'in içindeki daha derin bir tabloya mı ait?
fn within_nested_table(tr: ElementRef, table_el: ElementRef) -> bool {
    let mut cur = tr.parent();
    while let Some(node) = cur {
        let Some(pe) = ElementRef::wrap(node) else {
            break;
        };
        if pe == table_el {
            return false;
        }
        if pe.value().name() == "table" {
            return true;
        }
        cur = node.parent();
    }
    false
}

/// `tr`, `table_el`'e kadar olan yolda <thead> içinde mi?
fn in_thead(tr: ElementRef, table_el: ElementRef) -> bool {
    let mut cur = tr.parent();
    while let Some(node) = cur {
        let Some(pe) = ElementRef::wrap(node) else {
            break;
        };
        if pe == table_el {
            return false;
        }
        if pe.value().name() == "thead" {
            return true;
        }
        cur = node.parent();
    }
    false
}

/// İçerik gezgini: HTML ağacını sırayla gezer ve blokları üretir.
struct Walker<'a> {
    base: &'a str,
    images: Vec<String>,
}

impl<'a> Walker<'a> {
    fn new(base: &'a str) -> Self {
        Self {
            base,
            images: Vec::new(),
        }
    }

    fn walk(&mut self, node: NodeRef<Node>, out: &mut Vec<Block>, depth: u8) {
        let Some(el) = ElementRef::wrap(node) else {
            return;
        };
        if skip_element(&el) {
            return;
        }
        match el.value().name() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = el.value().name().as_bytes()[1] - b'0';
                let text = flat_text(&own_text(el));
                if !text.is_empty() {
                    out.push(Block::Heading { level, text });
                }
            }
            "p" => {
                let text = flat_text(&own_text(el));
                if !text.is_empty() {
                    out.push(Block::Paragraph(text));
                }
                // Paragraf içine gömülü görseller (WordPress sık yapar).
                for img in el.select(&img_selector()) {
                    if let Some(url) = img_in(img, &mut self.images, self.base) {
                        out.push(Block::Image(url));
                    }
                }
            }
            "pre" => {
                let text = own_text(el);
                let trimmed = text.trim_end_matches(['\n', ' ']).to_string();
                if !trimmed.trim().is_empty() {
                    out.push(Block::Code(trimmed));
                }
            }
            "blockquote" => {
                let text = flat_text(&own_text(el));
                if !text.is_empty() {
                    out.push(Block::Quote(text));
                }
            }
            "ul" | "ol" => self.list(el, out, depth),
            "li" => {
                let text = flat_text(&own_text(el));
                if !text.is_empty() {
                    out.push(Block::ListItem {
                        text,
                        ordered: false,
                        depth,
                    });
                }
            }
            "table" => {
                if let Some(t) = table_from(el) {
                    if !t.is_empty() && !is_layout_table(el, &t) {
                        out.push(Block::Table(t));
                        return;
                    }
                }
                // Düzen tablosu: içeriği normal akış olarak gez.
                for child in node.children() {
                    self.walk(child, out, depth);
                }
            }
            "figcaption" | "caption" => {
                let text = flat_text(&own_text(el));
                if !text.is_empty() {
                    out.push(Block::Caption(text));
                }
            }
            "img" => {
                if let Some(url) = img_in(el, &mut self.images, self.base) {
                    out.push(Block::Image(url));
                }
            }
            "hr" => out.push(Block::Divider),
            _ => {
                for child in node.children() {
                    self.walk(child, out, depth);
                }
            }
        }
    }

    /// <ul>/<ol> elemanını iç içe yapısıyla birlikte bloklara çevirir.
    fn list(&mut self, el: ElementRef, out: &mut Vec<Block>, depth: u8) {
        let ordered = el.value().name() == "ol";
        for child in el.children() {
            let Some(li) = ElementRef::wrap(child) else {
                continue;
            };
            if li.value().name() != "li" {
                // div ile sarmalanmış liste ögeleri
                if matches!(li.value().name(), "div" | "ul" | "ol") {
                    self.walk(*li, out, depth);
                }
                continue;
            }
            // Ögenin kendi metni: iç içe listeler hariç.
            let mut s = String::new();
            collect_text(*li, &mut s, &|e| matches!(e.value().name(), "ul" | "ol"));
            let text = flat_text(&s);
            if !text.is_empty() {
                out.push(Block::ListItem {
                    text,
                    ordered,
                    depth,
                });
            }
            for sub in li.children() {
                let Some(sub) = ElementRef::wrap(sub) else {
                    continue;
                };
                if matches!(sub.value().name(), "ul" | "ol") {
                    self.list(sub, out, depth.saturating_add(1));
                }
            }
        }
    }
}

fn img_selector() -> Selector {
    Selector::parse("img").expect("geçerli seçici")
}

/// HTML'den makale içeriğini çıkarır. `base` görsel URL'lerinin çözümü için kullanılır.
pub fn extract(
    html_text: &str,
    base: &str,
    opts: &ExtractOptions,
) -> Result<Article, ExtractError> {
    let html = Html::parse_document(html_text);
    let title = extract_title(&html);

    let (container, best_score) = best_container(&html, opts.min_text_len);
    let Some(container) = container else {
        return Err(if best_score == 0 {
            ExtractError::NoContent
        } else {
            ExtractError::TooThin {
                score: best_score,
                min: opts.min_text_len,
            }
        });
    };

    let mut blocks = Vec::new();
    let mut walker = Walker::new(base);
    walker.walk(*container, &mut blocks, 0);

    if blocks.is_empty() {
        return Err(ExtractError::NoBlocks);
    }

    drop_leading_title(&mut blocks, &title);
    trim_edges(&mut blocks);
    collapse_images(&mut blocks);

    if blocks.is_empty() {
        return Err(ExtractError::NoBlocks);
    }

    Ok(Article {
        title,
        blocks,
        images: walker.images,
    })
}

/// İlk blok belge başlığının tekrarıysa düşürür (başlık ayrıca yazılıyor).
fn drop_leading_title(blocks: &mut Vec<Block>, title: &str) {
    let text = match blocks.first() {
        Some(Block::Heading { text, .. }) | Some(Block::Paragraph(text)) => text.clone(),
        _ => return,
    };
    let a = clean_text(&text).to_lowercase();
    let b = clean_text(title).to_lowercase();
    if a.chars().count() < 12 || b.is_empty() {
        return;
    }
    let duplicate = a == b
        || (a.chars().count() >= 40 && b.contains(&a))
        || (b.chars().count() >= 40 && a.starts_with(&b))
        || (a.chars().count() >= 40 && a.starts_with(&b) && b.chars().count() >= 20);
    if duplicate {
        blocks.remove(0);
    }
}

/// Başta/sonda boşluk bloklarını (ayraç, tekrar eden başlık) temizler.
fn trim_edges(blocks: &mut Vec<Block>) {
    while matches!(blocks.last(), Some(Block::Divider)) {
        blocks.pop();
    }
    while matches!(blocks.first(), Some(Block::Divider)) {
        blocks.remove(0);
    }
}

/// Art arda gelen görselleri tekilledir ve ikili tekrarları atar.
fn collapse_images(blocks: &mut Vec<Block>) {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for b in blocks.drain(..) {
        match &b {
            Block::Image(url) => {
                if seen.contains(url) {
                    continue;
                }
                seen.push(url.clone());
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    *blocks = out;
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

    fn opts() -> ExtractOptions {
        ExtractOptions { min_text_len: 50 }
    }

    /// Uzun, tekrar eden bir paragraf: skor eşiğini aşmak için.
    fn filler() -> String {
        "Bu paragraf testlerde içerik skorunu yükseltmek için yazılmıştır ve \
         Türkçe karakterler içerir: ğüşıöçİĞÜŞÖÇ. Birkaç cümle daha ekliyoruz."
            .to_string()
    }

    fn article_html(inner: &str) -> String {
        format!(
            r#"<html><head><title>Deneme Başlık</title></head><body>
               <article><h1>Deneme Başlık</h1><p>{}</p>{inner}</article>
               </body></html>"#,
            filler()
        )
    }

    fn blocks_of(html: &str) -> Vec<Block> {
        extract(html, "https://site.com/yazi", &opts())
            .expect("çıkarım başarılı olmalı")
            .blocks
    }

    // --- temel metin yardımcıları ---

    #[test]
    fn clean_text_collapses_whitespace() {
        assert_eq!(clean_text("  a \n\t b  c "), "a b c");
        assert_eq!(clean_text(""), "");
        assert_eq!(clean_text("tek"), "tek");
    }

    #[test]
    fn clean_title_strips_site_suffix() {
        assert_eq!(
            clean_title("AutoGen Nedir? Derinlemesine İnceleyelim | Yazılım Blogu"),
            "AutoGen Nedir? Derinlemesine İnceleyelim"
        );
        // Ayraç yoksa başlık korunur.
        assert_eq!(clean_title("Kısa Başlık"), "Kısa Başlık");
        // Çok kısa ilk parça site adı olabilir; ayraç yok sayılır.
        assert_eq!(
            clean_title("Blog | Uzun Yazı Başlığı"),
            "Blog | Uzun Yazı Başlığı"
        );
    }

    #[test]
    fn extract_title_prefers_title_tag_then_h1_then_default() {
        let html = Html::parse_document(SAMPLE);
        assert_eq!(extract_title(&html), "Deneme Başlık");
        let h1 = Html::parse_document("<html><body><h1>H1 Başlık</h1></body></html>");
        assert_eq!(extract_title(&h1), "H1 Başlık");
        let empty = Html::parse_document("<html><body><p>x</p></body></html>");
        assert_eq!(extract_title(&empty), "Başlıksız belge");
    }

    const SAMPLE: &str = r#"
    <html><head><title>Deneme Başlık</title></head><body>
      <nav>Menü hiç kullanılmayacak</nav>
      <article>
        <h1>Deneme Başlık</h1>
        <p>Bu ilk paragraf ve içinde Türkçe karakterler var: ğüşıöçİĞÜŞÖÇ.</p>
        <p>İkinci paragraf biraz daha uzun olsun ki metin skorumuz iyi olsun ve
           çıkarım testinde gerçekten makale gövdesi seçilsin. Lorem ipsum dolor
           sit amet consectetur adipiscing elit sed do eiusmod tempor.</p>
        <pre><code>fn main() { println!("merhaba"); }</code></pre>
        <blockquote>Önemli bir alıntı cümlesi burada.</blockquote>
        <ul><li>Liste ögesi bir</li><li>Liste ögesi iki</li></ul>
        <img src="/img/macera.png" alt="macera">
        <hr>
        <h2>Alt başlık</h2>
        <p>Kapanış paragrafı.</p>
      </article>
      <aside>Reklam kenar çubuğu</aside>
    </body></html>"#;

    #[test]
    fn full_pipeline_blocks_and_images() {
        let art = extract(SAMPLE, "https://ornek.com/yazi", &opts()).unwrap();
        assert_eq!(art.title, "Deneme Başlık");
        // h1, belge başlığının tekrarı olduğu için düşürülür.
        assert!(!matches!(
            art.blocks.first(),
            Some(Block::Heading { level: 1, .. })
        ));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("ğüşıöç"))));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Code(c) if c.contains("println"))));
        assert!(art.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::ListItem { .. })));
        assert!(art.blocks.iter().any(|b| matches!(b, Block::Divider)));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Image(u) if u == "https://ornek.com/img/macera.png")));
        assert_eq!(
            art.images,
            vec!["https://ornek.com/img/macera.png".to_string()]
        );
    }

    #[test]
    fn nav_and_aside_subtrees_are_fully_skipped() {
        // Eski hata: skip listesi yalnızca elemanı atlıyordu, çocukları
        // (nav içindeki <p>) içeriğe sızıyordu.
        let html = article_html(
            r#"<nav><p>MENÜ İÇİ PARAGRAF</p></nav>
               <aside><p>YAN PANEL PARAGRAF</p></aside>
               <form><p>FORM PARAGRAF</p></form>
               <footer><p>ALT BİLGİ PARAGRAF</p></footer>"#,
        );
        let blocks = blocks_of(&html);
        let joined = format!("{blocks:?}");
        assert!(!joined.contains("MENÜ İÇİ"));
        assert!(!joined.contains("YAN PANEL"));
        assert!(!joined.contains("FORM PARAGRAF"));
        assert!(!joined.contains("ALT BİLGİ"));
    }

    #[test]
    fn boilerplate_containers_are_pruned() {
        let html = article_html(
            r#"<div class="post-related"><p>İLGİSİZ YAZI BAŞLIĞI</p></div>
               <div class="sharedaddy share-buttons"><p>PAYLAŞ</p></div>
               <section id="comments"><p>YORUM FORMU METNİ</p></section>
               <div class="widget sidebar-box"><p>KENAR ÇUBUĞU</p></div>"#,
        );
        let joined = format!("{:?}", blocks_of(&html));
        for bad in ["İLGİSİZ", "PAYLAŞ", "YORUM FORMU", "KENAR ÇUBUĞU"] {
            assert!(!joined.contains(bad), "{bad} sızmamalı");
        }
    }

    #[test]
    fn display_none_and_hidden_elements_are_skipped() {
        let html = article_html(
            r#"<p style="display: none">GİZLİ SATIR</p>
               <p hidden>DE GİZLİ</p>
               <p aria-hidden="true">ARIA GİZLİ</p>"#,
        );
        let joined = format!("{:?}", blocks_of(&html));
        for bad in ["GİZLİ SATIR", "DE GİZLİ", "ARIA GİZLİ"] {
            assert!(!joined.contains(bad));
        }
    }

    #[test]
    fn article_beats_broad_wrapper_container() {
        // Gerçek dünya hatası: <main class="content"> makaleyi + yan içeriği
        // kapsadığı için en yüksek skoru alıp seçiliyordu.
        let html = format!(
            r#"<html><head><title>Kapsayıcı Testi | Site Adı</title></head><body>
               <main class="content" id="content">
                 <article class="post entry">
                   <h1>Kapsayıcı Testi</h1>
                   <p>{}</p>
                 </article>
                 <section id="comments"><p>YORUM FORMU</p></section>
                 <div class="post-related"><p>İLGİSİZ YAZI</p></div>
               </main></body></html>"#,
            filler()
        );
        let art = extract(&html, "https://site.com/y", &opts()).unwrap();
        assert_eq!(art.title, "Kapsayıcı Testi");
        let joined = format!("{:?}", art.blocks);
        assert!(!joined.contains("YORUM FORMU"));
        assert!(!joined.contains("İLGİSİZ"));
    }

    #[test]
    fn falls_back_to_structural_container_for_unknown_template() {
        // Bilinen hiçbir sınıf yok (portfolyo/açılış sayfası gibi).
        let html = format!(
            r#"<html><head><title>Portfolyo</title></head><body>
               <div id="page"><section class="hero">
                 <h2>Merhaba</h2><p>{}</p>
               </section>
               <section class="skills"><p>{}</p></section></div>
               </body></html>"#,
            filler(),
            filler()
        );
        let art = extract(&html, "https://site.com/", &opts()).unwrap();
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("paragraf"))));
    }

    #[test]
    fn plain_body_without_wrappers_still_extracts() {
        let html = format!(
            "<html><head><title>Düz Sayfa</title></head><body><p>{}</p><p>ikinci paragraf</p></body></html>",
            filler()
        );
        let art = extract(&html, "https://site.com/", &opts()).unwrap();
        assert!(art.blocks.len() >= 2);
    }

    // --- tablolar ---

    #[test]
    fn table_with_th_header_becomes_header_row() {
        let html = article_html(
            r#"<table>
                 <tr><th>Başlık A</th><th>Başlık B</th></tr>
                 <tr><td>a1</td><td>b1</td></tr>
                 <tr><td>a2</td><td>b2</td></tr>
               </table>"#,
        );
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .expect("tablo çıkarılmalı");
        assert_eq!(table.header, vec!["Başlık A", "Başlık B"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0], vec!["a1", "b1"]);
        assert_eq!(table.columns, 2);
    }

    #[test]
    fn thead_marks_header_and_body_rows_are_kept() {
        let html = article_html(
            r#"<table>
                 <thead><tr><th>A</th><th>B</th></tr></thead>
                 <tbody><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody>
               </table>"#,
        );
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.header, vec!["A", "B"]);
        assert_eq!(table.rows, vec![vec!["1", "2"], vec!["3", "4"]]);
    }

    #[test]
    fn table_without_header_returns_body_rows_only() {
        let html = article_html("<table><tr><td>a</td><td>b</td></tr></table>");
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(table.header.is_empty());
        assert_eq!(table.rows, vec![vec!["a", "b"]]);
    }

    #[test]
    fn table_cells_keep_lists_and_br_as_separate_lines() {
        let html = article_html(
            r#"<table>
                 <tr><th>A</th><th>B</th></tr>
                 <tr>
                   <td><ul><li>bir</li><li>iki</li></ul></td>
                   <td>satır bir<br>satır iki</td>
                 </tr>
               </table>"#,
        );
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.rows[0][0], "• bir\n• iki");
        assert_eq!(table.rows[0][1], "satır bir\nsatır iki");
    }

    #[test]
    fn ragged_rows_are_padded_to_column_count() {
        let html = article_html(
            r#"<table><tr><th>A</th><th>B</th><th>C</th></tr>
               <tr><td>1</td></tr><tr><td>1</td><td>2</td><td>3</td></tr></table>"#,
        );
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.columns, 3);
        assert_eq!(table.rows[0], vec!["1", "", ""]);
    }

    #[test]
    fn nested_table_is_not_duplicated_as_its_own_table() {
        let html = article_html(
            r#"<table><tr><td>dış <table><tr><td>iç</td></tr></table></td><td>x</td></tr></table>"#,
        );
        let tables: Vec<Table> = blocks_of(&html)
            .into_iter()
            .filter_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tables.len(), 1, "iç içe tablo ayrı tablo olmamalı");
        assert_eq!(tables[0].columns, 2);
        assert!(tables[0].rows[0][0].contains("iç"));
    }

    #[test]
    fn layout_table_is_flattened_into_reading_flow() {
        // Eski tip siteler sayfa iskeletini yüzlerce satırlık bir tabloyla
        // kurar (blog.rust-lang.org gibi). Izgara olarak basmak yerine içerik
        // sırayla gezilmeli.
        let rows: String = (0..40)
            .map(|i| format!("<tr><td>Gönderi {i}</td><td>2026-01-{i:02}</td></tr>"))
            .collect();
        let html = article_html(&format!("<table>{rows}</table>"));
        let blocks = blocks_of(&html);
        assert!(
            !blocks.iter().any(|b| matches!(b, Block::Table(_))),
            "düzen tablosu ızgara olarak basılmamalı"
        );
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::ListItem { .. } | Block::Paragraph(_))),
            "içeriği erişlebilir olmalı"
        );
    }

    #[test]
    fn large_table_with_header_is_still_rendered_as_table() {
        let rows: String = (0..40)
            .map(|i| format!("<tr><td>a{i}</td><td>b{i}</td></tr>"))
            .collect();
        let html = article_html(&format!(
            "<table><thead><tr><th>Ad</th><th>Değer</th></tr></thead><tbody>{rows}</tbody></table>"
        ));
        let table = blocks_of(&html)
            .into_iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .expect("başlıklı büyük tablo korunmalı");
        assert_eq!(table.header, vec!["Ad", "Değer"]);
        assert_eq!(table.rows.len(), 40);
    }

    #[test]
    fn empty_table_produces_no_block() {
        let html = article_html("<table></table>");
        assert!(!blocks_of(&html)
            .iter()
            .any(|b| matches!(b, Block::Table(_))));
    }

    // --- listeler ---

    #[test]
    fn ordered_and_nested_lists_keep_depth_and_kind() {
        let html =
            article_html(r#"<ol><li>birinci</li><li>ikinci<ul><li>alt madde</li></ul></li></ol>"#);
        let items: Vec<(String, bool, u8)> = blocks_of(&html)
            .into_iter()
            .filter_map(|b| match b {
                Block::ListItem {
                    text,
                    ordered,
                    depth,
                } => Some((text, ordered, depth)),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![
                ("birinci".to_string(), true, 0),
                ("ikinci".to_string(), true, 0),
                ("alt madde".to_string(), false, 1),
            ]
        );
    }

    #[test]
    fn unordered_list_items_are_not_ordered() {
        let html = article_html("<ul><li>madde bir</li><li>madde iki</li></ul>");
        let items: Vec<bool> = blocks_of(&html)
            .into_iter()
            .filter_map(|b| match b {
                Block::ListItem { ordered, .. } => Some(ordered),
                _ => None,
            })
            .collect();
        assert_eq!(items, vec![false, false]);
    }

    /// Site platformu senaryoları: her platformun tipik DOM iskeleti,
    /// gerçek dünyada görüldüğü şekliyle sadeleştirilmiş haliyle.
    mod platforms {
        use super::*;

        fn first_paragraph(html: &str) -> String {
            let art =
                extract(html, "https://site.com/yazi", &opts()).expect("çıkarım başarılı olmalı");
            assert!(!art.title.is_empty());
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
            assert!(first_paragraph(html).contains("WordPress"));
        }

        #[test]
        fn wordpress_hueman_entry_inner() {
            let html = r#"
            <html><head><title>Hueman Yazı</title></head><body>
              <main class="content" id="content">
                <article class="post">
                  <div class="entry themeform"><div class="entry-inner">
                    <p>Hueman teması içerik alanı.</p>
                  </div></div>
                </article>
              </main></body></html>"#;
            assert!(first_paragraph(html).contains("Hueman"));
        }

        #[test]
        fn ghost_gh_content() {
            let html = r#"
            <html><head><title>Ghost Yazı</title></head><body>
              <article class="post"><section class="gh-content">
                <p>Ghost resmi teması içerik bölümü.</p>
              </section></article>
            </body></html>"#;
            assert!(first_paragraph(html).contains("Ghost"));
        }

        #[test]
        fn medium_and_substack_markup() {
            let medium = r#"
            <html><head><title>Medium Yazı</title></head><body>
              <article><section class="articleBody">
                <p>Medium gövde paragrafı.</p>
              </section></article>
            </body></html>"#;
            assert!(first_paragraph(medium).contains("Medium"));

            let substack = r#"
            <html><head><title>Substack Yazı</title></head><body>
              <div class="available-content"><div class="body markup">
                <p>Substack bülten gövdesi.</p>
              </div></div>
            </body></html>"#;
            assert!(first_paragraph(substack).contains("Substack"));
        }

        #[test]
        fn docusaurus_and_mkdocs() {
            let docusaurus = r#"
            <html><head><title>Docusaurus</title></head><body>
              <article><div class="theme-doc-markdown">
                <p>Docusaurus doküman sayfası içeriği burada.</p>
              </div></article>
            </body></html>"#;
            assert!(first_paragraph(docusaurus).contains("Docusaurus"));

            let mkdocs = r#"
            <html><head><title>MkDocs</title></head><body>
              <main><div class="md-content">
                <p>MkDocs Material içerik alanı.</p>
              </div></main>
            </body></html>"#;
            assert!(first_paragraph(mkdocs).contains("MkDocs"));
        }

        #[test]
        fn read_the_docs_rst_content() {
            let html = r#"
            <html><head><title>Sphinx</title></head><body>
              <div class="wy-nav-content"><div class="rst-content">
                <p>Sphinx/Read the Docs gövdesi.</p>
              </div></div>
            </body></html>"#;
            assert!(first_paragraph(html).contains("Sphinx"));
        }

        #[test]
        fn vitepress_and_hugo() {
            let vitepress = r#"
            <html><head><title>VitePress</title></head><body>
              <main><div class="vp-doc">
                <p>VitePress doküman paragrafı.</p>
              </div></main>
            </body></html>"#;
            assert!(first_paragraph(vitepress).contains("VitePress"));

            let hugo = r#"
            <html><head><title>Hugo</title></head><body>
              <main><article class="post"><div class="post__content">
                <p>Hugo tema içerik alanı.</p>
              </div></article></main>
            </body></html>"#;
            assert!(first_paragraph(hugo).contains("Hugo"));
        }

        #[test]
        fn sidebar_text_does_not_raise_wrapper_score() {
            let html = r#"
            <html><head><title>Skor</title></head><body>
              <div class="content">
                <aside class="sidebar"><p>kısa link listesi</p></aside>
                <article><p>{}</p></article>
              </div>
            </body></html>"#;
            let long = "Uzun makale paragrafı ".repeat(20);
            let html = html.replace("{}", &long);
            let art = extract(&html, "https://s.com", &opts()).unwrap();
            assert!(art
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Paragraph(t) if t.contains("Uzun makale"))));
            assert!(!format!("{:?}", art.blocks).contains("kısa link listesi"));
        }
    }

    // --- görseller ve başlıklar ---

    #[test]
    fn inline_image_inside_paragraph_is_kept() {
        let html = article_html(r#"<p>Metin ve görsel<img src="/g1.png"></p>"#);
        let art = extract(&html, "https://site.com/y", &opts()).unwrap();
        let has_para = art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("Metin ve görsel")));
        let has_img = art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Image(u) if u == "https://site.com/g1.png"));
        assert!(has_para && has_img, "{:?}", art.blocks);
    }

    #[test]
    fn figure_and_figcaption_become_image_and_caption() {
        let html = article_html(
            r#"<figure><img src="/f.png"><figcaption>Şekil 1: örnek</figcaption></figure>"#,
        );
        let blocks = blocks_of(&html);
        assert!(blocks.iter().any(|b| matches!(b, Block::Image(_))));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Caption(t) if t.contains("Şekil 1"))));
    }

    #[test]
    fn duplicate_images_are_collapsed() {
        let html = article_html(r#"<img src="/a.png"><p>ara</p><img src="/a.png">"#);
        let art = extract(&html, "https://site.com/y", &opts()).unwrap();
        let count = art
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Image(_)))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn lazy_loading_attributes_are_used() {
        let frag = Html::parse_fragment(r#"<img data-src="/lazy.png">"#);
        let mut images = Vec::new();
        let got = img_in(frag.root_element(), &mut images, "https://b.com/post");
        assert_eq!(got.as_deref(), Some("https://b.com/lazy.png"));
        assert_eq!(images, vec!["https://b.com/lazy.png"]);
    }

    #[test]
    fn srcset_is_used_when_src_missing() {
        let frag = Html::parse_fragment(r#"<img srcset="/kucuk.png 1x, /buyuk.png 2x">"#);
        let mut images = Vec::new();
        let got = img_in(frag.root_element(), &mut images, "https://b.com/post");
        assert_eq!(got.as_deref(), Some("https://b.com/kucuk.png"));
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
    fn leading_title_duplicate_is_dropped() {
        let html = format!(
            r#"<html><head><title>Uzun Bir Yazı Başlığı | Site</title></head><body>
               <article><h1>Uzun Bir Yazı Başlığı</h1><p>{}</p></article></body></html>"#,
            filler()
        );
        let art = extract(&html, "https://s.com/y", &opts()).unwrap();
        assert_eq!(art.title, "Uzun Bir Yazı Başlığı");
        assert!(!matches!(
            art.blocks.first(),
            Some(Block::Heading { text, .. }) if text == "Uzun Bir Yazı Başlığı"
        ));
    }

    // --- hatalar ---

    #[test]
    fn thin_content_is_rejected_with_score() {
        let html = "<html><body><div id=\"content\"><p>kısa</p></div></body></html>";
        let strict = ExtractOptions { min_text_len: 200 };
        let err = extract(html, "https://a.com", &strict).unwrap_err();
        let ExtractError::TooThin { score, min } = err else {
            panic!("TooThin bekleniyordu, gelen: {err}");
        };
        assert!(score < min);
        assert!(score > 0);
    }

    #[test]
    fn empty_document_reports_no_content() {
        let html = "<html><body><script>var x = 1;</script></body></html>";
        let err = extract(html, "https://a.com", &opts()).unwrap_err();
        assert!(matches!(err, ExtractError::NoContent));
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
    fn boilerplate_markers_respect_word_boundaries() {
        // Gerçek dünya hatası: "modal" işareti `tag-multimodalmessage` sınıfına
        // takılıp makalenin tamamını eliyordu.
        assert!(!has_boilerplate_marker(
            "post-27846 post type-post hentry tag-multimodalmessage tag-conversation"
        ));
        // Advert ve türevleri yakalanmalı.
        assert!(has_boilerplate_marker("advertisement-box"));
        assert!(has_boilerplate_marker("cookies-notice"));
        assert!(has_boilerplate_marker("widget_text"));
        assert!(has_boilerplate_marker("hu-rel-post-thumb"));
        assert!(has_boilerplate_marker("post-related"));
        assert!(has_boilerplate_marker("social-share-buttons"));
        assert!(has_boilerplate_marker("sr-only"));
        // İlgisiz sınıflar etkilenmemeli.
        assert!(!has_boilerplate_marker("entry-inner"));
        assert!(!has_boilerplate_marker("markdown-body"));
        assert!(!has_boilerplate_marker("unrelatedly-named"));
    }

    #[test]
    fn article_class_with_many_tags_is_not_boilerplate() {
        // WordPress makale sınıfları onlarca `tag-` içerir; bunlardan biri
        // şüpheli bir kelimeye benzese bile makale elenmemeli.
        let html = format!(
            r#"<html><head><title>Modal Pencereler | Site</title></head><body>
               <main class="content"><article class="post hentry tag-multimodal tag-modal-dialog">
                 <h1>Modal Pencereler</h1><p>{}</p>
               </article></main></body></html>"#,
            filler()
        );
        let art = extract(&html, "https://s.com/y", &opts()).unwrap();
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("paragraf"))));
    }

    #[test]
    fn tightest_container_within_threshold_wins() {
        // Geniş sarmalayıcı makaleden biraz daha fazla metin içeriyor; yine de
        // asıl içerik kabı seçilmeli ("kategori/etiket" satırı sızmamalı).
        let html = format!(
            r#"<html><head><title>Dar Kap | Site</title></head><body>
               <main class="content"><div class="page-title"><p>KATEGORİ ETİKET SATIRI</p></div>
                 <article class="post"><h1>Dar Kap</h1><div class="entry-inner">
                   <p>{}</p>
                 </div></article>
               </main></body></html>"#,
            filler().repeat(6)
        );
        let art = extract(&html, "https://s.com/y", &opts()).unwrap();
        assert!(!format!("{:?}", art.blocks).contains("KATEGORİ ETİKET"));
        assert!(art
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t.contains("paragraf"))));
    }

    #[test]
    fn text_score_ignores_boilerplate_and_rewards_paragraphs() {
        let with_p = Html::parse_document("<div><p>abcdef</p><p>ghijkl</p></div>");
        let with_span = Html::parse_document("<div><span>abcdefghijkl</span></div>");
        assert!(text_score(with_p.root_element()) > text_score(with_span.root_element()));

        let boiler = Html::parse_document(
            "<div><p>gerçek içerik</p><div class=\"post-related\"><p>uzun uzun uzun uzun</p></div></div>",
        );
        let clean = Html::parse_document("<div><p>gerçek içerik</p></div>");
        assert_eq!(
            text_score(boiler.root_element()),
            text_score(clean.root_element())
        );
    }
}
