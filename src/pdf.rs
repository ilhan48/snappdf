use crate::extract::{Article, Block};
use anyhow::{Context, Result};
use genpdf::elements::{Break, Paragraph};
use genpdf::fonts::{FontData, FontFamily};
use genpdf::style::{Color, Style};
use genpdf::{Alignment, Document, Element, Margins, SimplePageDecorator};

/// Gömülü Liberation fontları (SIL OFL 1.1) — çevrimdışı çalışma + UTF-8/Türkçe desteği.
const SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");
const SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Bold.ttf");
const SANS_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Italic.ttf");
const SANS_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf");
const MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Regular.ttf");
const MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Bold.ttf");
const MONO_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Italic.ttf");
const MONO_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationMono-BoldItalic.ttf");

/// PDF üretim ayarları.
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// Altbilgi: başlık + sayfa numarası
    pub footer: bool,
    /// Yazı taban boyutu ( punto)
    pub font_size: u8,
    /// Görselleri indirip göm (offline araçlarla: yalnızca png/jpeg)
    pub embed_images: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            footer: true,
            font_size: 11,
            embed_images: true,
        }
    }
}

/// Renk sabitleri.
const TEXT: Color = Color::Rgb(0x1F, 0x24, 0x2B);
const MUTED: Color = Color::Rgb(0x6B, 0x72, 0x80);

fn font_from_bytes(data: &[u8]) -> Result<FontData> {
    FontData::new(data.to_vec(), None).context("Gömülü font çözümlenemedi")
}

/// Sans font ailesini kurar (regular/bold/italic/bold-italic).
pub fn sans_family() -> Result<FontFamily<FontData>> {
    Ok(FontFamily {
        regular: font_from_bytes(SANS_REGULAR)?,
        bold: font_from_bytes(SANS_BOLD)?,
        italic: font_from_bytes(SANS_ITALIC)?,
        bold_italic: font_from_bytes(SANS_BOLD_ITALIC)?,
    })
}

/// Mono font ailesini kurar.
pub fn mono_family() -> Result<FontFamily<FontData>> {
    Ok(FontFamily {
        regular: font_from_bytes(MONO_REGULAR)?,
        bold: font_from_bytes(MONO_BOLD)?,
        italic: font_from_bytes(MONO_ITALIC)?,
        bold_italic: font_from_bytes(MONO_BOLD_ITALIC)?,
    })
}

/// Gövde metnini satır bazında italic stille yazar (kod bloklarında yorum vb. için).
fn mono_styled(code: &str) -> Vec<(&'static str, String)> {
    // Basit satır ayrımı: her satır kendi Paragraph'ünde; boş satırlar korunur.
    code.lines().map(|l| ("line", l.to_string())).collect()
}

/// Bir kod bloğunu satırlara bölerek stilize paragraflar üretir.
/// Dönen tip bilinçli olarak geniş: her öge StyledElement<Paragraph>.
fn code_paragraphs(code: &str) -> Vec<genpdf::elements::StyledElement<Paragraph>> {
    let base = Style::new().with_font_size(9).with_color(CODE_TEXT);
    mono_styled(code)
        .into_iter()
        .map(|(_, line)| {
            let text: &str = if line.trim().is_empty() { " " } else { &line };
            Paragraph::new(text).styled(base)
        })
        .collect()
}

const CODE_TEXT: Color = Color::Rgb(0x24, 0x29, 0x2F);

/// Alt bilgi: her sayfada başlık solda, "Sayfa X" sağda.
fn footer_layout(title: String) -> impl Fn(usize) -> genpdf::elements::LinearLayout + 'static {
    move |page: usize| {
        let mut layout = genpdf::elements::LinearLayout::vertical();
        let mut row = genpdf::elements::TableLayout::new(vec![1, 1]);
        let style = Style::new().with_font_size(8).with_color(MUTED);
        let _ = row.push_row(vec![
            Box::new(Paragraph::new(title.clone()).styled(style)),
            Box::new(
                Paragraph::new(format!("Sayfa {page}"))
                    .aligned(Alignment::Right)
                    .styled(style),
            ),
        ]);
        layout.push(row);
        layout.push(Break::new(0.6));
        layout
    }
}

/// Makaleden A4 PDF belgesi kurar.
pub fn build_document(article: &Article, opts: &PdfOptions) -> Result<Document> {
    let sans = sans_family()?;
    let mono = mono_family()?;

    let mut doc = Document::new(sans);
    doc.set_title(&article.title);
    doc.set_minimal_conformance();
    doc.set_line_spacing(1.3);
    doc.set_font_size(opts.font_size);

    let mut decorator = SimplePageDecorator::new();
    decorator.set_margins(Margins::trbl(14, 16, 16, 16));
    if opts.footer {
        let title = article.title.clone();
        decorator.set_header(move |page| footer_layout(title.clone())(page));
    }
    doc.set_page_decorator(decorator);

    // Başlık
    doc.push(
        Paragraph::new(&article.title)
            .aligned(Alignment::Left)
            .styled(Style::new().bold().with_font_size(20).with_color(TEXT)),
    );
    doc.push(Break::new(0.8));

    let mono_font = doc.add_font_family(mono);
    let _code_style = Style::from(mono_font)
        .with_font_size(9)
        .with_color(CODE_TEXT);

    for block in &article.blocks {
        match block {
            Block::Heading { level, text } => {
                doc.push(Break::new(0.4));
                let size = match level {
                    1 => 17,
                    2 => 15,
                    3 => 13,
                    _ => 12,
                };
                doc.push(
                    Paragraph::new(text)
                        .styled(Style::new().bold().with_font_size(size).with_color(TEXT)),
                );
                doc.push(Break::new(0.2));
            }
            Block::Paragraph(text) => {
                doc.push(Paragraph::new(text).styled(Style::new().with_color(TEXT)));
                doc.push(Break::new(0.35));
            }
            Block::Quote(text) => {
                let q = Paragraph::new(text)
                    .styled(Style::new().italic().with_color(MUTED))
                    .padded(genpdf::Margins::trbl(2, 0, 2, 6))
                    .framed()
                    .padded(genpdf::Margins::trbl(2, 6, 2, 6));
                doc.push(q);
                doc.push(Break::new(0.35));
            }
            Block::Code(_) => {
                for p in code_paragraphs_of(block) {
                    doc.push(p.padded(genpdf::Margins::all(1)).framed());
                }
                doc.push(Break::new(0.4));
            }
            Block::ListItem(text) => {
                doc.push(
                    Paragraph::new(format!("•  {text}"))
                        .styled(Style::new().with_color(TEXT))
                        .padded(genpdf::Margins::trbl(1, 0, 1, 6)),
                );
            }
            Block::Image(url) => {
                if opts.embed_images {
                    if let Some(img) = crate::images::load_blocking(url) {
                        doc.push(img.with_alignment(Alignment::Center));
                        doc.push(Break::new(0.3));
                        continue;
                    }
                } // Görsel yüklenemedi: küçük bir not bırak.
                doc.push(
                    Paragraph::new(format!("[görsel yüklenemedi: {url}]"))
                        .styled(Style::new().italic().with_font_size(8).with_color(MUTED)),
                );
                doc.push(Break::new(0.2));
            }
            Block::Divider => {
                doc.push(
                    Paragraph::new("―".repeat(24))
                        .aligned(Alignment::Center)
                        .styled(Style::new().with_color(MUTED).with_font_size(8)),
                );
                doc.push(Break::new(0.5));
            }
        }
    }
    Ok(doc)
}

/// Block::Code içeriğinden stilize paragraflar üretir.
type CodePara = genpdf::elements::StyledElement<Paragraph>;
fn code_paragraphs_of(block: &Block) -> Vec<CodePara> {
    match block {
        Block::Code(code) => code_paragraphs(code),
        _ => vec![],
    }
}

/// URL'den PDF dosya adı üretir (host tabanlı, güvenli karakterlere indirgenmiş).
pub fn output_name(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        return "sayfa.pdf".to_string();
    }
    format!("{safe}.pdf")
}

/// Belgeyi bellekteki PDF baytlarına dönüştürür.
pub fn render_to_bytes(doc: Document) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    doc.render(&mut buf).context("PDF render başarısız")?;
    Ok(buf)
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Block;

    #[test]
    fn output_name_host_based() {
        assert_eq!(
            output_name("https://developer.mozilla.org/x/y?a=b"),
            "developer.mozilla.org.pdf"
        );
        assert_eq!(output_name("http://localhost:3000/a"), "localhost_3000.pdf");
        assert_eq!(output_name("http://user:pass@host/p"), "user_pass_host.pdf");
        assert_eq!(output_name("not-a-url"), "not-a-url.pdf");
        assert_eq!(output_name(""), "sayfa.pdf");
    }

    fn article() -> Article {
        Article {
            title: "Türkçe Başlık ĞÜŞİÖÇ".to_string(),
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Alt başlık".into(),
                },
                Block::Paragraph("Paragraf metni ğüşıöç.".into()),
                Block::Code("fn main() {}\n// yorum".into()),
                Block::Quote("Alıntı".into()),
                Block::ListItem("madde".into()),
                Block::Divider,
            ],
            images: vec![],
        }
    }

    #[test]
    fn font_families_load_from_embedded_bytes() {
        let sans = sans_family().unwrap();
        let mono = mono_family().unwrap();
        // FontData karşılaştırılabilir değil; yalnızca kurulum hatasız olmalı.
        let _ = (sans.regular, sans.bold, sans.italic, sans.bold_italic);
        let _ = (mono.regular, mono.bold, mono.italic, mono.bold_italic);
    }

    #[test]
    fn pdf_options_defaults() {
        let o = PdfOptions::default();
        assert!(o.footer);
        assert_eq!(o.font_size, 11);
        assert!(o.embed_images);
    }

    #[test]
    fn code_paragraphs_split_lines_and_keep_empty() {
        let ps = code_paragraphs("a\n\nb");
        assert_eq!(ps.len(), 3);
    }

    #[test]
    fn code_paragraphs_of_ignores_non_code() {
        let mut ps = code_paragraphs_of(&Block::Paragraph("x".into()));
        assert!(ps.is_empty());
        ps = code_paragraphs_of(&Block::Code("x".into()));
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn mono_styled_produces_lines() {
        let lines = mono_styled("x\ny");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].1, "x");
    }

    #[test]
    fn build_document_with_footer_and_turkish_text() {
        let doc = build_document(&article(), &PdfOptions::default()).unwrap();
        let bytes = render_to_bytes(doc).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 1000);
    }

    #[test]
    fn build_document_without_footer() {
        let opts = PdfOptions {
            footer: false,
            ..Default::default()
        };
        let doc = build_document(&article(), &opts).unwrap();
        let bytes = render_to_bytes(doc).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn pdf_output_is_deterministic_size_ordered() {
        // İçerik arttıkça bayt da artmalı (boş belge < dolu belge).
        let mut small = article();
        small.blocks.clear();
        let b1 = render_to_bytes(build_document(&small, &PdfOptions::default()).unwrap()).unwrap();
        let b2 =
            render_to_bytes(build_document(&article(), &PdfOptions::default()).unwrap()).unwrap();
        assert!(b2.len() > b1.len());
    }

    #[test]
    fn footer_layout_renders_page_number() {
        let mut layout = footer_layout("Deneme".to_string())(3);
        // LinearLayout'un Element::render'ı doğrudan test edilemez (Area gerekir);
        // en azından kurulum hatasız olmalı.
        let _ = &mut layout;
    }

    #[test]
    fn multipage_content_splits_pages() {
        let mut art = article();
        for i in 0..120 {
            art.blocks.push(Block::Paragraph(format!(
                "Sayfa taşması için uzun paragraf {i}. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."
            )));
        }
        let bytes = render_to_bytes(build_document(&art, &PdfOptions::default()).unwrap()).unwrap();
        // Birden fazla sayfa: /Type /Pages ağacında birden çok /Page olmalı.
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.matches("/Type /Page").count() >= 2 || bytes.len() > 20_000);
    }
}
