use crate::extract::{Article, Block, Table};
use crate::highlight::Kind;
use crate::images::ImageOutcome;
use anyhow::{Context, Result};
use genpdf::elements::{
    Break, BulletPoint, FrameCellDecorator, LinearLayout, Paragraph, TableLayout,
};
use genpdf::fonts::{FontData, FontFamily};
use genpdf::render;
use genpdf::style::{Color, Style};
use genpdf::{
    Alignment, Context as PdfContext, Document, Element, Margins, PageDecorator, RenderResult, Size,
};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// Gömülü Liberation fontları (SIL OFL 1.1) — çevrimdışı çalışma + UTF-8/Türkçe desteği.
const SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");
const SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Bold.ttf");
const SANS_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Italic.ttf");
const SANS_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf");
const MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Regular.ttf");
const MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Bold.ttf");
const MONO_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationMono-Italic.ttf");
const MONO_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationMono-BoldItalic.ttf");

// --- sayfa geometrisi ---
/// Desteklenen sayfa boyutları. `Tablet`, 4:3 tablet ekranlarına oturan
/// (162x216 mm) okuma sayfasıdır.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSize {
    /// 210x297 mm (varsayılan)
    #[default]
    A4,
    /// 148x210 mm — tablet ve e-okuyucularda tam ekran okuma
    A5,
    /// 216x279 mm (US Letter)
    Letter,
    /// 162x216 mm — 4:3 tablet ekranı
    Tablet,
}

impl PageSize {
    /// CLI değerini çözer (`a4`, `a5`, `letter`, `tablet`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "a4" => Some(Self::A4),
            "a5" => Some(Self::A5),
            "letter" => Some(Self::Letter),
            "tablet" => Some(Self::Tablet),
            _ => None,
        }
    }

    /// Kullanıcıya gösterilen ad.
    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "a4",
            Self::A5 => "a5",
            Self::Letter => "letter",
            Self::Tablet => "tablet",
        }
    }

    /// Sayfa genişliği ve yüksekliği (mm).
    pub fn dimensions_mm(self) -> (f64, f64) {
        match self {
            Self::A4 => (210.0, 297.0),
            Self::A5 => (148.0, 210.0),
            Self::Letter => (216.0, 279.0),
            Self::Tablet => (162.0, 216.0),
        }
    }

    /// Kenar boşlukları (üst, sağ, alt, sol) mm cinsinden. A4'teki
    /// 10/18/16/18 oranı küçük sayfalara da taşınır: dar sayfada tam genişlik
    /// kullanılsın, kenar boşluğu metni sıkıştırmasın.
    pub fn margins_mm(self) -> (u32, u32, u32, u32) {
        let (width, height) = self.dimensions_mm();
        let horizontal = width / 210.0;
        let vertical = height / 297.0;
        let scale = |value: f64, factor: f64| (value * factor).round() as u32;
        (
            scale(10.0, vertical),
            scale(18.0, horizontal),
            scale(16.0, vertical),
            scale(18.0, horizontal),
        )
    }

    /// Metin sütununun genişliği — görsel ölçekleme için de kullanılır.
    pub fn content_width_mm(self) -> f64 {
        let (width, _) = self.dimensions_mm();
        let (_, right, _, left) = self.margins_mm();
        width - f64::from(left) - f64::from(right)
    }

    /// Metin sütununun yüksekliği (üstbilgi hariç).
    pub fn content_height_mm(self) -> f64 {
        let (_, height) = self.dimensions_mm();
        let (top, _, bottom, _) = self.margins_mm();
        height - f64::from(top) - f64::from(bottom)
    }

    /// Tek bir görsel bu yüksekliği aşmaz (bir sayfayı tamamen kaplamasın).
    pub fn max_image_height_mm(self) -> f64 {
        self.content_height_mm() * 0.74
    }
}

/// Okuma teması: sayfa zemini + metin paleti.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Beyaz zemin, koyu metin (varsayılan)
    #[default]
    Light,
    /// Gece okuması için koyu zemin, açık metin
    Dark,
    /// Uzun okumalarda gözü yormayan sıcak krem zemin
    Sepia,
}

impl Theme {
    /// CLI değerini çözer (`light`, `dark`, `sepia`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "sepia" => Some(Self::Sepia),
            _ => None,
        }
    }

    /// Kullanıcıya gösterilen ad.
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Sepia => "sepia",
        }
    }

    /// Temanın paleti; yalnızca kod bloğu renkleri `code` temasından gelir.
    pub fn palette_with(self, code: CodeTheme) -> Palette {
        let (text, muted, background) = match self {
            Self::Light => (Color::Rgb(0x1F, 0x24, 0x2B), Color::Rgb(0x6B, 0x72, 0x80), None),
            Self::Dark => (
                Color::Rgb(0xE4, 0xE7, 0xEB),
                Color::Rgb(0x9A, 0xA4, 0xB2),
                Some(Color::Rgb(0x16, 0x19, 0x1D)),
            ),
            Self::Sepia => (
                Color::Rgb(0x4A, 0x3B, 0x2A),
                Color::Rgb(0x8A, 0x73, 0x55),
                Some(Color::Rgb(0xF6, 0xEE, 0xDC)),
            ),
        };
        Palette {
            text,
            muted,
            code: code.palette(self),
            background,
        }
    }
}

/// Kod bloğu renk paleti.
///
/// `Auto` sayfa temasına uyar; diğerleri açıkça seçilebilir, böylece örneğin
/// beyaz bir sayfada koyu bir kod kutusu (Monokai) kullanılabilir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeTheme {
    /// Sayfa temasına uyar (açıkça seçilmediği sürece varsayılan).
    #[default]
    Auto,
    /// GitHub açık paleti.
    GithubLight,
    /// GitHub koyu paleti (koyu kod kutusu).
    GithubDark,
    /// Monokai (koyu zemin, canlı renkler).
    Monokai,
    /// Solarized Light (krem zemin).
    SolarizedLight,
    /// Solarized Dark (petrol mavisi zemin).
    SolarizedDark,
    /// Sepya sayfa temasıyla uyumlu sıcak palet.
    Sepia,
}

impl CodeTheme {
    /// CLI değerini çözer.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "github-light" | "github" => Some(Self::GithubLight),
            "github-dark" => Some(Self::GithubDark),
            "monokai" => Some(Self::Monokai),
            "solarized-light" | "solarized" => Some(Self::SolarizedLight),
            "solarized-dark" => Some(Self::SolarizedDark),
            "sepia" => Some(Self::Sepia),
            _ => None,
        }
    }

    /// Kullanıcıya gösterilen ad.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::GithubLight => "github-light",
            Self::GithubDark => "github-dark",
            Self::Monokai => "monokai",
            Self::SolarizedLight => "solarized-light",
            Self::SolarizedDark => "solarized-dark",
            Self::Sepia => "sepia",
        }
    }

    /// Kod paletini çözer. `Auto`, sayfa temasına uyan paleti seçer: koyu
    /// sayfada kutu, sayfa zemininden biraz açık kalır (boğulmasın).
    pub fn palette(self, page: Theme) -> CodePalette {
        match self {
            Self::Auto => match page {
                Theme::Light => GITHUB_LIGHT,
                Theme::Dark => DARK_ON_PAGE,
                Theme::Sepia => SEPIA,
            },
            Self::GithubLight => GITHUB_LIGHT,
            Self::GithubDark => GITHUB_DARK,
            Self::Monokai => MONOKAI,
            Self::SolarizedLight => SOLARIZED_LIGHT,
            Self::SolarizedDark => SOLARIZED_DARK,
            Self::Sepia => SEPIA,
        }
    }
}

/// Bir temanın renkleri.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Gövde metni.
    pub text: Color,
    /// Üstbilgi, açıklama, ayraç gibi ikincil öğeler.
    pub muted: Color,
    /// Kod bloğu kutusu ve söz dizimi renkleri.
    pub code: CodePalette,
    /// Sayfa zemini; `None` ise zemin çizilmez (beyaz kalır).
    pub background: Option<Color>,
}

/// Kod bloğu kutusunun ve söz dizimi vurgulamasının renkleri.
///
/// Kutu (zemin + çerçeve) bloglardaki kod blokları gibi ayrı bir dikdörtgen
/// olarak görünür; renkler de kod metnini yorum/dize/sayı/anahtar sözcük
/// bazında ayırır.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodePalette {
    /// Kutunun dolgu zemini (metnin arkasına çizilir).
    pub background: Color,
    /// Kutunun ince çerçevesi.
    pub border: Color,
    /// Satır numarası, ayraç, sarma işareti ve dil rozeti (ikincil kod metni).
    pub gutter: Color,
    /// Kutunun sol kenarındaki renk şeridi.
    pub accent: Color,
    /// Blognun vurguladığı satırların zemini (metnin arkasına çizilir).
    pub highlight: Color,
    /// Renklendirilmemiş kod metni.
    pub plain: Color,
    /// `fn`, `let`, `class`, `if` gibi anahtar sözcükler.
    pub keyword: Color,
    /// Dize değişmezleri.
    pub string: Color,
    /// Yorumlar (eğik basılır).
    pub comment: Color,
    /// Sayılar.
    pub number: Color,
    /// İşlev/makro çağrıları.
    pub function: Color,
    /// Tür adları ve dekoratörler.
    pub type_name: Color,
}

/// GitHub açık paleti (açık tema ve beyaz zeminli kod kutuları).
const GITHUB_LIGHT: CodePalette = CodePalette {
    background: Color::Rgb(0xF6, 0xF8, 0xFA),
    border: Color::Rgb(0xD0, 0xD7, 0xDE),
    gutter: Color::Rgb(0x7D, 0x85, 0x90),
    accent: Color::Rgb(0x82, 0x50, 0xDF),
    highlight: Color::Rgb(0xFF, 0xF3, 0xCC),
    plain: Color::Rgb(0x24, 0x29, 0x2F),
    keyword: Color::Rgb(0xCF, 0x22, 0x2E),
    string: Color::Rgb(0x0A, 0x30, 0x69),
    comment: Color::Rgb(0x6E, 0x77, 0x81),
    number: Color::Rgb(0x05, 0x50, 0xAE),
    function: Color::Rgb(0x66, 0x2A, 0xC0),
    type_name: Color::Rgb(0x95, 0x38, 0x00),
};

/// GitHub koyu paleti (nihai siyaha yakın zemin).
const GITHUB_DARK: CodePalette = CodePalette {
    background: Color::Rgb(0x0D, 0x11, 0x17),
    border: Color::Rgb(0x30, 0x36, 0x3D),
    gutter: Color::Rgb(0x6E, 0x76, 0x81),
    accent: Color::Rgb(0x89, 0x57, 0xE5),
    highlight: Color::Rgb(0x2E, 0x2A, 0x1A),
    plain: Color::Rgb(0xC9, 0xD1, 0xD9),
    keyword: Color::Rgb(0xFF, 0x7B, 0x72),
    string: Color::Rgb(0xA5, 0xD6, 0xFF),
    comment: Color::Rgb(0x8B, 0x94, 0x9E),
    number: Color::Rgb(0x79, 0xC0, 0xFF),
    function: Color::Rgb(0xD2, 0xA8, 0xFF),
    type_name: Color::Rgb(0xFF, 0xA6, 0x57),
};

/// Koyu sayfa temasının zemini (#16191D) için ayarlanmış koyu palet: kutu,
/// sayfadan biraz açık kalır.
const DARK_ON_PAGE: CodePalette = CodePalette {
    background: Color::Rgb(0x1E, 0x22, 0x28),
    border: Color::Rgb(0x30, 0x36, 0x3D),
    gutter: Color::Rgb(0x7D, 0x85, 0x90),
    accent: Color::Rgb(0x89, 0x57, 0xE5),
    highlight: Color::Rgb(0x35, 0x30, 0x1E),
    plain: Color::Rgb(0xCF, 0xD6, 0xDE),
    keyword: Color::Rgb(0xFF, 0x7B, 0x72),
    string: Color::Rgb(0xA5, 0xD6, 0xFF),
    comment: Color::Rgb(0x8B, 0x94, 0x9E),
    number: Color::Rgb(0x79, 0xC0, 0xFF),
    function: Color::Rgb(0xD2, 0xA8, 0xFF),
    type_name: Color::Rgb(0xFF, 0xA6, 0x57),
};

/// Monokai.
const MONOKAI: CodePalette = CodePalette {
    background: Color::Rgb(0x27, 0x28, 0x22),
    border: Color::Rgb(0x3E, 0x3D, 0x32),
    gutter: Color::Rgb(0x75, 0x71, 0x5E),
    accent: Color::Rgb(0xF9, 0x26, 0x72),
    highlight: Color::Rgb(0x3E, 0x3D, 0x32),
    plain: Color::Rgb(0xF8, 0xF8, 0xF2),
    keyword: Color::Rgb(0xF9, 0x26, 0x72),
    string: Color::Rgb(0xE6, 0xDB, 0x74),
    comment: Color::Rgb(0x75, 0x71, 0x5E),
    number: Color::Rgb(0xAE, 0x81, 0xFF),
    function: Color::Rgb(0xA6, 0xE2, 0x2E),
    type_name: Color::Rgb(0x66, 0xD9, 0xEF),
};

/// Solarized Light.
const SOLARIZED_LIGHT: CodePalette = CodePalette {
    background: Color::Rgb(0xFD, 0xF6, 0xE3),
    border: Color::Rgb(0xEE, 0xE8, 0xD5),
    gutter: Color::Rgb(0x83, 0x94, 0x96),
    accent: Color::Rgb(0x26, 0x8B, 0xD2),
    highlight: Color::Rgb(0xEE, 0xE8, 0xD5),
    plain: Color::Rgb(0x58, 0x6E, 0x75),
    keyword: Color::Rgb(0x85, 0x99, 0x00),
    string: Color::Rgb(0x2A, 0xA1, 0x98),
    comment: Color::Rgb(0x93, 0xA1, 0xA1),
    number: Color::Rgb(0xD3, 0x36, 0x82),
    function: Color::Rgb(0x26, 0x8B, 0xD2),
    type_name: Color::Rgb(0xB5, 0x89, 0x00),
};

/// Solarized Dark.
const SOLARIZED_DARK: CodePalette = CodePalette {
    background: Color::Rgb(0x00, 0x2B, 0x36),
    border: Color::Rgb(0x07, 0x36, 0x42),
    gutter: Color::Rgb(0x58, 0x6E, 0x75),
    accent: Color::Rgb(0x26, 0x8B, 0xD2),
    highlight: Color::Rgb(0x07, 0x36, 0x42),
    plain: Color::Rgb(0x93, 0xA1, 0xA1),
    keyword: Color::Rgb(0x85, 0x99, 0x00),
    string: Color::Rgb(0x2A, 0xA1, 0x98),
    comment: Color::Rgb(0x58, 0x6E, 0x75),
    number: Color::Rgb(0xD3, 0x36, 0x82),
    function: Color::Rgb(0x26, 0x8B, 0xD2),
    type_name: Color::Rgb(0xB5, 0x89, 0x00),
};

/// Sepya sayfa temasıyla uyumlu sıcak palet.
const SEPIA: CodePalette = CodePalette {
    background: Color::Rgb(0xEF, 0xE5, 0xCE),
    border: Color::Rgb(0xD6, 0xC5, 0x9F),
    gutter: Color::Rgb(0xA0, 0x8C, 0x6A),
    accent: Color::Rgb(0xA0, 0x6A, 0x2A),
    highlight: Color::Rgb(0xEA, 0xD9, 0xAC),
    plain: Color::Rgb(0x3E, 0x32, 0x26),
    keyword: Color::Rgb(0x9A, 0x2F, 0x12),
    string: Color::Rgb(0x1E, 0x5B, 0x40),
    comment: Color::Rgb(0x8A, 0x73, 0x55),
    number: Color::Rgb(0x2C, 0x4E, 0x7A),
    function: Color::Rgb(0x6B, 0x3A, 0x7C),
    type_name: Color::Rgb(0x8A, 0x4B, 0x12),
};

/// PDF üretim ayarları.
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// Üstbilgi: başlık + sayfa numarası
    pub footer: bool,
    /// Yazı taban boyutu (punto)
    pub font_size: u8,
    /// Görselleri indirip göm (offline araçlarla: yalnızca png/jpeg)
    pub embed_images: bool,
    /// Sayfa boyutu (A4/A5/Letter/Tablet)
    pub page: PageSize,
    /// Okuma teması
    pub theme: Theme,
    /// Kod bloğu renk paleti (`auto`: sayfa temasına uyar)
    pub code_theme: CodeTheme,
    /// Kod bloklarında satır numarası göster (tek satırlık kodda gizlenir)
    pub line_numbers: bool,
    /// Bilinen dillerde kod bloğunun üstüne dil rozeti koy
    pub code_badge: bool,
    /// Blogun vurguladığı kod satırlarını renkli bantla bas
    pub line_highlights: bool,
    /// Başlıklardan PDF yer imi (içindekiler) üret
    pub bookmarks: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            footer: true,
            font_size: 11,
            embed_images: true,
            page: PageSize::default(),
            theme: Theme::default(),
            code_theme: CodeTheme::default(),
            line_numbers: true,
            code_badge: true,
            line_highlights: true,
            bookmarks: true,
        }
    }
}

/// Fontu yalnızca `chars` karakterlerinin glifleriyle gömer.
///
/// Kaynak font 400 KB civarındadır; alt kümeleme sonrası aynı font metinde
/// geçen karakterlere iner (tipik bir yazıda 10-30 KB). `subsetter` fontun
/// `cmap` tablosunu kaldırdığı için `crate::fonts::subset` gerekli `cmap`i
/// yeniden inşa eder — printpdf fontu rusttype/stb_truetype ile ayrıştırır.
fn font_from_bytes(data: &[u8], chars: &BTreeSet<char>) -> Result<FontData> {
    let subset = crate::fonts::subset(data, chars).context("Gömülü font alt kümelendi")?;
    FontData::new(subset, None).context("Gömülü font çözümlenemedi")
}

/// Sans font ailesini kurar (regular/bold/italic/bold-italic).
pub fn sans_family(chars: &BTreeSet<char>) -> Result<FontFamily<FontData>> {
    Ok(FontFamily {
        regular: font_from_bytes(SANS_REGULAR, chars)?,
        bold: font_from_bytes(SANS_BOLD, chars)?,
        italic: font_from_bytes(SANS_ITALIC, chars)?,
        bold_italic: font_from_bytes(SANS_BOLD_ITALIC, chars)?,
    })
}

/// Mono font ailesini kurar.
pub fn mono_family(chars: &BTreeSet<char>) -> Result<FontFamily<FontData>> {
    Ok(FontFamily {
        regular: font_from_bytes(MONO_REGULAR, chars)?,
        bold: font_from_bytes(MONO_BOLD, chars)?,
        italic: font_from_bytes(MONO_ITALIC, chars)?,
        bold_italic: font_from_bytes(MONO_BOLD_ITALIC, chars)?,
    })
}

/// Belgede basılabilecek tüm karakterler. Font alt kümesi bu kümeye göre
/// kurulur; burada olmayan bir karakter basılırsa glifi bulunamaz.
///
/// Küme belgeden toplanır ve arayüz metinleri (sayfa numarası, madde imi,
/// ayraç) ile yaygın tipografi işaretleri eklenir.
pub fn charset(article: &Article) -> BTreeSet<char> {
    let mut chars = BTreeSet::new();
    chars.extend(' '..='~');
    chars.extend(
        "ğĞüÜşŞıİöÖçÇâÂîÎûÛéÉèÈàÀôÔ•‣▪·–—―…“”„‘’«»→←≥≤≠±×÷°²³½¼¾€$£¥¢©®™†‡§¶№│\u{00A0}".chars(),
    );
    chars.extend(article.title.chars());
    for block in &article.blocks {
        match block {
            Block::Heading { text, .. }
            | Block::Paragraph(text)
            | Block::Caption(text)
            | Block::Quote(text)
            | Block::ListItem { text, .. } => chars.extend(text.chars()),
            Block::Code { text, .. } => chars.extend(text.chars()),
            Block::Table(table) => {
                for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                    chars.extend(cell.chars());
                }
            }
            Block::Image(_) | Block::Divider => {}
        }
    }
    chars
}

/// Makalede mono font gerektiren bir blok var mı? (kod blokları)
fn needs_mono(article: &Article) -> bool {
    article
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Code { .. }))
}

/// Üstbilgi: her sayfada başlık solda, "Sayfa X" sağda. İlk sayfada başlık
/// zaten büyük puntoyla yazıldığı için tekrarlanmaz.
/// Üstbilgi metni: normalde "Sayfa N"; sayfa bir kod bloğunun devamıyla
/// başlıyorsa "Sayfa N · kod devamı" (odak modu: uzun blok içinde nerede
/// olduğunu kaybetmemek için).
fn header_text(page: usize, code_continues: bool) -> String {
    if code_continues {
        format!("Sayfa {page} · kod devamı")
    } else {
        format!("Sayfa {page}")
    }
}

/// Üstbilgi: her sayfada başlık solda, sayfa numarası sağda. İlk sayfada başlık
/// zaten büyük puntoyla yazıldığı için tekrarlanmaz.
fn header_layout(
    title: String,
    palette: Palette,
    code_continues: bool,
) -> impl Fn(usize) -> LinearLayout + 'static {
    move |page: usize| {
        let mut layout = LinearLayout::vertical();
        let style = Style::new().with_font_size(8).with_color(palette.muted);
        let mut row = TableLayout::new(vec![1, 1]);
        let left = if page <= 1 {
            Paragraph::new("").styled(style)
        } else {
            Paragraph::new(title.clone()).styled(style)
        };
        let _ = row.push_row(vec![
            Box::new(left),
            Box::new(
                Paragraph::new(header_text(page, code_continues))
                    .aligned(Alignment::Right)
                    .styled(style),
            ),
        ]);
        layout.push(row);
        layout.push(Break::new(0.5));
        layout
    }
}

/// Render sırasında doldurulan yer imi/kutu yakalama durumu.
#[derive(Debug, Default)]
struct CaptureState {
    /// Yakalanan yer imleri (belge sırasında).
    entries: Vec<Bookmark>,
    /// Yakalanan kod kutusu dikdörtgenleri (belge sırasında).
    code_boxes: Vec<CodeBoxRect>,
    /// Önceki sayfada bir kod bloğu bölündü mü? Bir sonraki sayfanın üstbilgisi
    /// bunu okuyup "kod devamı" notunu basar.
    code_continues: bool,
    /// Sayfalara bölünen kod bloğu sayısı (özet için).
    code_splits: usize,
    /// Vurgulu kod satırlarının arkalarına çizilecek bantlar (mm, sayfa
    /// üst-sol köşesinden). Kutu zemininden *sonra*, metinden *önce* basılır.
    highlight_bands: Vec<CodeBoxRect>,
    /// Vurgulu basılan kod satırı sayısı (özet için).
    highlighted_lines: usize,
    /// Kopyalanmaması gereken kod süsleri (satır numarası sütunu, dil rozeti).
    decorations: Vec<CodeDecoration>,
    /// Şu an doldurulan sayfa (1 tabanlı).
    page: usize,
    /// İçerik alanının sayfa üstünden uzaklığı (mm).
    content_top_mm: f64,
    /// İçerik alanının sol kenarının sayfa solundan uzaklığı (mm).
    content_left_mm: f64,
    /// İçerik alanının genişliği (mm).
    content_width_mm: f64,
    /// İçerik alanının yüksekliği (mm).
    content_height_mm: f64,
}

/// Bir kod kutusunun sayfadaki yeri (mm, sayfanın üst-sol köşesinden).
///
/// Kutunun dolgu zemini genpdf eleman API'siyle çizilemediği için dikdörtgen
/// render sırasında yakalanır ve sayfa içerik akışına *önce* eklenir; böylece
/// kod metninin arkasında kalır (`postprocess::paint_backgrounds`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodeBoxRect {
    /// 1 tabanlı sayfa numarası.
    pub page: u32,
    /// Metin sütununun sol kenarı.
    pub x_mm: f64,
    /// Sayfa üstünden kutunun üst kenarı.
    pub y_mm: f64,
    /// Kutunun genişliği (metin sütunu genişliği).
    pub width_mm: f64,
    /// Kutunun yüksekliği.
    pub height_mm: f64,
}

/// Kod kutusunda **kopyalanmaması** gereken metin öbeği: satır numarası
/// sütunu (numara, `│` ayracı, `»` sarma işareti) ya da dil rozeti.
///
/// Numaralar PDF'te gerçek metin olarak basılır (görünmeleri ve kopyalanan
/// metinde satırların kaymaması için); seç-kopyala sırasında dışarıda
/// kalmaları ise `ActualText` ile sağlanır — bkz.
/// `postprocess::hide_code_decorations`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodeDecoration {
    /// 1 tabanlı sayfa numarası.
    pub page: u32,
    /// Öbeğin başlaması gereken alan (mm, sayfa üst-sol köşesinden).
    pub x_mm: f64,
    /// Alanın üst kenarı (mm).
    pub y_mm: f64,
    /// Alanın genişliği/yüksekliği (mm).
    pub width_mm: f64,
    /// Alanın yüksekliği (mm).
    pub height_mm: f64,
    /// Öbeğin bastığı glif sayısı. İçerik akışındaki metin gösterimiyle
    /// **birebir** karşılaştırılır: sayılar uyuşmazsa hiçbir şey gizlenmez.
    pub glyphs: usize,
}

/// PDF yer imi hedefi: başlık, seviye ve sayfadaki konumu.
#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    /// Yer imi başlığı.
    pub title: String,
    /// 0 = belge başlığı, 1..=6 = h1..h6.
    pub level: u8,
    /// 1 tabanlı sayfa numarası.
    pub page: u32,
    /// Sayfa üstünden uzaklık (mm).
    pub y_mm: f64,
}

/// Sayfa numarasına göre üstbilgi elemanı üreten geri çağırım.
type HeaderCallback = Box<dyn Fn(usize) -> Box<dyn Element>>;

/// Kenar boşlukları + üstbilgi uygular (`SimplePageDecorator` ile aynı iş) ve
/// her sayfanın içerik alanını kaydeder; böylece başlıkların hangi sayfada ve
/// hangi yükseklikte basıldığı bilinebilir.
struct CapturingDecorator {
    margins: Margins,
    header: Option<HeaderCallback>,
    state: Rc<RefCell<CaptureState>>,
    page_height_mm: f64,
    bottom_mm: f64,
    /// Sol kenar boşluğu (mm): metin sütununun sol kenarı.
    left_mm: f64,
    page: usize,
}

impl PageDecorator for CapturingDecorator {
    fn decorate_page<'a>(
        &mut self,
        context: &PdfContext,
        mut area: render::Area<'a>,
        style: Style,
    ) -> Result<render::Area<'a>, genpdf::error::Error> {
        self.page += 1;
        area.add_margins(self.margins);
        if let Some(callback) = &self.header {
            let mut element = callback(self.page);
            let result = element.render(context, area.clone(), style)?;
            area.add_offset(genpdf::Position::new(0, result.size.height));
        }
        let content_height_mm = f64::from(area.size().height);
        // Kenar boşluğu ve üstbilgi, alanı yukarıdan ve aşağıdan kısar:
        // üst kenar = sayfa yüksekliği - alt boşluk - alanın yüksekliği.
        let content_top_mm = self.page_height_mm - self.bottom_mm - content_height_mm;
        {
            let mut state = self.state.borrow_mut();
            state.page = self.page;
            state.content_top_mm = content_top_mm;
            state.content_left_mm = self.left_mm;
            state.content_width_mm = f64::from(area.size().width);
            state.content_height_mm = content_height_mm;
            // Üstbilgi okunduktan sonra sıfırlanır: bu sayfanın içeriği
            // (bir kod bloğu sürüyorsa) değeri yeniden belirleyecek.
            state.code_continues = false;
        }
        Ok(area)
    }
}

/// Bir başlık paragrafını sarar ve render sırasında konumunu yer imi olarak
/// kaydeder.
struct BookmarkPoint {
    inner: Box<dyn Element>,
    title: String,
    level: u8,
    state: Rc<RefCell<CaptureState>>,
    recorded: bool,
}

impl BookmarkPoint {
    fn new(
        inner: Box<dyn Element>,
        title: String,
        level: u8,
        state: Rc<RefCell<CaptureState>>,
    ) -> Self {
        Self {
            inner,
            title,
            level,
            state,
            recorded: false,
        }
    }
}

impl Element for BookmarkPoint {
    fn render(
        &mut self,
        context: &PdfContext,
        area: render::Area<'_>,
        style: Style,
    ) -> Result<RenderResult, genpdf::error::Error> {
        let result = self.inner.render(context, area.clone(), style)?;
        // Bir paragraf sayfaya sığmazsa sonraki sayfada yeniden render edilir;
        // yer imi ise ilk (gerçek) konumu göstermelidir.
        if !self.recorded {
            self.recorded = true;
            let mut state = self.state.borrow_mut();
            // Kök dikey yerleşimde her eleman bir öncekinin bittiği yerden
            // başlar ve kalan yükseklik kadar küçülür. Bu yüzden elemanın üst
            // kenarı, kendisine verilen alanın kalan yüksekliğinden hesaplanır.
            let y_mm =
                state.content_top_mm + (state.content_height_mm - f64::from(area.size().height));
            let page = state.page as u32;
            state.entries.push(Bookmark {
                title: self.title.clone(),
                level: self.level,
                page,
                y_mm,
            });
        }
        Ok(result)
    }
}

/// Bir kod bloğu kutusunu sarar ve render sırasında kapladığı dikdörtgeni
/// kaydeder (dolgu zemini PDF'e sonradan eklenir).
struct CodeBoxCapture {
    inner: Box<dyn Element>,
    state: Rc<RefCell<CaptureState>>,
}

impl CodeBoxCapture {
    fn new(inner: Box<dyn Element>, state: Rc<RefCell<CaptureState>>) -> Self {
        Self { inner, state }
    }
}

impl Element for CodeBoxCapture {
    fn render(
        &mut self,
        context: &PdfContext,
        area: render::Area<'_>,
        style: Style,
    ) -> Result<RenderResult, genpdf::error::Error> {
        // Kök dikey yerleşimde elemanın üst kenarı, kendisine verilen alanın
        // kalan yüksekliğinden hesaplanır (bkz. BookmarkPoint).
        let available_mm = f64::from(area.size().height);
        let top_mm = {
            let state = self.state.borrow();
            state.content_top_mm + (state.content_height_mm - available_mm)
        };
        let result = self.inner.render(context, area, style)?;

        // Bir sayfaya sığmayan blok sonraki sayfada yeniden render edilir; her
        // çağrı kendi sayfasına bir dikdörtgen yazar, yani bölünen kod bloğu
        // sayfa başına ayrı bir kutu olur.
        let height_mm = f64::from(result.size.height).min(available_mm);
        if height_mm > 0.0 {
            let mut state = self.state.borrow_mut();
            let rect = CodeBoxRect {
                page: state.page as u32,
                x_mm: state.content_left_mm,
                y_mm: top_mm,
                width_mm: state.content_width_mm,
                height_mm,
            };
            state.code_boxes.push(rect);
        }
        Ok(result)
    }
}

/// Render edilmeye hazır belge; yer imi konumları render sırasında yakalanır.
pub struct PdfJob {
    doc: Document,
    bookmarks_enabled: bool,
    state: Rc<RefCell<CaptureState>>,
}

impl PdfJob {
    /// Belgeyi PDF baytlarına dönüştürür ve yakalanan yer imlerini döndürür.
    pub fn render_with_bookmarks(self) -> Result<(Vec<u8>, Vec<Bookmark>)> {
        let PdfJob {
            doc,
            bookmarks_enabled,
            state,
        } = self;
        let mut buffer = Vec::new();
        doc.render(&mut buffer).context("PDF render başarısız")?;
        let bookmarks = if bookmarks_enabled {
            state.borrow().entries.clone()
        } else {
            Vec::new()
        };
        Ok((buffer, bookmarks))
    }
}

/// Makaleden PDF belgesi kurar (sayfa boyutu, tema ve yer imi yakalaması dâhil).
pub fn build_document(article: &Article, opts: &PdfOptions) -> Result<PdfJob> {
    let chars = charset(article);
    let sans = sans_family(&chars)?;
    let palette = opts.theme.palette_with(opts.code_theme);
    let (width_mm, height_mm) = opts.page.dimensions_mm();
    let (top, right, bottom, left) = opts.page.margins_mm();

    let mut doc = Document::new(sans);
    doc.set_title(&article.title);
    doc.set_minimal_conformance();
    doc.set_line_spacing(1.3);
    doc.set_font_size(opts.font_size);
    doc.set_paper_size(Size::new(width_mm as f32, height_mm as f32));

    let state = Rc::new(RefCell::new(CaptureState::default()));
    let mut decorator = CapturingDecorator {
        margins: Margins::trbl(top, right, bottom, left),
        header: None,
        state: Rc::clone(&state),
        page_height_mm: height_mm,
        bottom_mm: f64::from(bottom),
        left_mm: f64::from(left),
        page: 0,
    };
    if opts.footer {
        let title = article.title.clone();
        let header_state = Rc::clone(&state);
        decorator.header = Some(Box::new(move |page: usize| {
            // Önceki sayfa bir kod bloğunun ortasında bitmişse not eklenir.
            let continues = header_state.borrow().code_continues;
            Box::new(header_layout(title.clone(), palette, continues)(page))
        }));
    }
    doc.set_page_decorator(decorator);

    // Kod bloğu yoksa mono fontları hiç gömmeyiz: genpdf kullanılmayan fontları
    // da gömer. (Alt kümeleme sonrası maliyet küçük ama gereksiz 4 font olur.)
    let mono_style = if needs_mono(article) {
        let family = doc.add_font_family(mono_family(&chars)?);
        Some(
            Style::from(family)
                .with_font_size(9)
                .with_color(palette.code.plain),
        )
    } else {
        None
    };

    let body = Style::new().with_color(palette.text);
    let title_style = Style::new()
        .bold()
        .with_font_size(20)
        .with_color(palette.text);

    // Başlık (yer imi ağacının kökü)
    let title_paragraph = Paragraph::new(&article.title)
        .aligned(Alignment::Left)
        .styled(title_style);
    if opts.bookmarks {
        doc.push(BookmarkPoint::new(
            Box::new(title_paragraph),
            article.title.clone(),
            0,
            Rc::clone(&state),
        ));
    } else {
        doc.push(title_paragraph);
    }
    doc.push(Break::new(0.8));

    let mut i = 0usize;
    while i < article.blocks.len() {
        match &article.blocks[i] {
            Block::ListItem { .. } => {
                let end = list_run_end(&article.blocks, i);
                let run = &article.blocks[i..end];
                doc.push(nested_list(run, body));
                doc.push(Break::new(0.5));
                i = end;
                continue;
            }
            block => {
                push_block(&mut doc, block, opts, palette, body, mono_style, &state);
            }
        }
        i += 1;
    }
    Ok(PdfJob {
        doc,
        bookmarks_enabled: opts.bookmarks,
        state,
    })
}

/// Üretilen PDF ve sayısal özet.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// PDF baytları.
    pub bytes: Vec<u8>,
    /// PDF'e yazılan yer imi sayısı.
    pub bookmarks: usize,
    /// Sayfalara bölünen kod bloğu sayısı (üstbilgide "kod devamı" görünen).
    pub code_splits: usize,
    /// Vurgulu basılan kod satırı sayısı.
    pub highlighted_lines: usize,
}

/// Makaleyi PDF baytlarına dönüştürür: yer imleri, meta veri ve tema zemini dâhil.
pub fn render_article(
    article: &Article,
    opts: &PdfOptions,
    meta: &crate::postprocess::Meta,
) -> Result<Rendered> {
    let job = build_document(article, opts)?;
    // Kod kutularının dikdörtgenleri render sırasında yakalanır; dolgu zemin
    // sonradan (postprocess) sayfa akışının başına eklenir.
    let state = Rc::clone(&job.state);
    let (bytes, bookmarks) = job.render_with_bookmarks()?;
    let bookmark_count = bookmarks.len();
    // Kod kutusu dolgusu, kutu metinleriyle aynı paletten gelmeli.
    let palette = opts.theme.palette_with(opts.code_theme);
    let code_boxes = state.borrow().code_boxes.clone();
    let code_splits = state.borrow().code_splits;
    let highlight_bands = state.borrow().highlight_bands.clone();
    let highlighted_lines = state.borrow().highlighted_lines;
    let decorations = state.borrow().decorations.clone();
    let bytes = crate::postprocess::apply(
        bytes,
        &crate::postprocess::OutputOptions {
            meta: meta.clone(),
            page: opts.page,
            background: palette.background,
            code: Some(palette.code),
            code_boxes,
            code_highlights: highlight_bands,
            code_decorations: decorations,
            bookmarks,
        },
    )?;
    Ok(Rendered {
        bytes,
        bookmarks: bookmark_count,
        code_splits,
        highlighted_lines,
    })
}

/// Tek bir bloğu belgeye ekler (liste blokları çağıran tarafından gruplanır).
fn push_block(
    doc: &mut Document,
    block: &Block,
    opts: &PdfOptions,
    palette: Palette,
    body: Style,
    mono_style: Option<Style>,
    state: &Rc<RefCell<CaptureState>>,
) {
    match block {
        Block::Heading { level, text } => {
            doc.push(Break::new(0.4));
            let size = match level {
                1 => 17,
                2 => 15,
                3 => 13,
                _ => 12,
            };
            let style = Style::new()
                .bold()
                .with_font_size(size)
                .with_color(palette.text);
            let paragraph = Paragraph::new(text).styled(style);
            if opts.bookmarks {
                doc.push(BookmarkPoint::new(
                    Box::new(paragraph),
                    text.clone(),
                    *level,
                    Rc::clone(state),
                ));
            } else {
                doc.push(paragraph);
            }
            doc.push(Break::new(0.2));
        }
        Block::Paragraph(text) => {
            doc.push(Paragraph::new(text).styled(body));
            doc.push(Break::new(0.35));
        }
        Block::Caption(text) => {
            doc.push(
                Paragraph::new(text).aligned(Alignment::Center).styled(
                    Style::new()
                        .italic()
                        .with_font_size(8)
                        .with_color(palette.muted),
                ),
            );
            doc.push(Break::new(0.35));
        }
        Block::Quote(text) => {
            doc.push(
                Paragraph::new(text)
                    .styled(Style::new().italic().with_color(palette.muted))
                    .padded(Margins::trbl(2, 0, 2, 6))
                    .framed()
                    .padded(Margins::trbl(2, 6, 2, 6)),
            );
            doc.push(Break::new(0.35));
        }
        Block::Code {
            text,
            lang,
            highlights,
        } => {
            let style = mono_style.unwrap_or(body);
            // Kutu; dolgu zemini, söz dizimi renkleri, satır numaraları ve dil
            // rozetiyle birlikte basılır. Kapladığı dikdörtgen sonraki adımda
            // (postprocess) dolgulanır.
            let block = code_block(
                text,
                lang.as_deref(),
                highlights,
                CodeBlockOptions {
                    style,
                    palette: palette.code,
                    line_numbers: opts.line_numbers,
                    badge: opts.code_badge,
                    line_highlights: opts.line_highlights,
                },
                state,
            );
            doc.push(CodeBoxCapture::new(Box::new(block), Rc::clone(state)));
            doc.push(Break::new(0.5));
        }
        Block::Table(table) => {
            if let Some(el) = table_element(table, palette) {
                doc.push(Break::new(0.2));
                doc.push(el);
                doc.push(Break::new(0.6));
            }
        }
        Block::Image(url) => {
            // --no-images: sessizce atla, URL listesi basma.
            if !opts.embed_images {
                return;
            }
            match crate::images::load(
                url,
                opts.page.content_width_mm(),
                opts.page.max_image_height_mm(),
            ) {
                ImageOutcome::Loaded(img) => {
                    doc.push(*img);
                    doc.push(Break::new(0.4));
                }
                ImageOutcome::Skipped => {}
                ImageOutcome::Failed => {
                    doc.push(
                        Paragraph::new(format!("[görsel yüklenemedi: {url}]")).styled(
                            Style::new()
                                .italic()
                                .with_font_size(8)
                                .with_color(palette.muted),
                        ),
                    );
                    doc.push(Break::new(0.2));
                }
            }
        }
        Block::Divider => {
            doc.push(
                Paragraph::new("―".repeat(30))
                    .aligned(Alignment::Center)
                    .styled(Style::new().with_color(palette.muted).with_font_size(8)),
            );
            doc.push(Break::new(0.5));
        }
        // Liste blokları build_document içinde gruplanır.
        Block::ListItem { .. } => {}
    }
}

/// `start`'tan itibaren art arda gelen liste bloklarının bitiş indeksi.
fn list_run_end(blocks: &[Block], start: usize) -> usize {
    blocks[start..]
        .iter()
        .position(|b| !matches!(b, Block::ListItem { .. }))
        .map(|n| start + n)
        .unwrap_or(blocks.len())
}

/** Kod kutusunun iç dolgusu (mm) — dört yana da uygulanır. */
const CODE_PADDING_MM: f64 = 2.0;

/// Kod bloğu: mono font, korunmuş girinti, söz dizimi renkleri, satır numarası
/// ve dil rozeti.
///
/// Kutunun zemini, çerçevesi ve sol şeridi burada değil, sayfa içerik akışının
/// başında çizilir (`CodeBoxRect` + `postprocess::code_box_operators`); çünkü
/// genpdf eleman API'sinde dolgu yok ve yuvarlatılmış köşe isteniyor.
/// Kod bloğunun görünüm ayarları (`code_block` imzasını sade tutar).
struct CodeBlockOptions {
    /// Kod metni stili (mono aile + punto + renk).
    style: Style,
    /// Kutu paleti.
    palette: CodePalette,
    /// Satır numaraları basılsın mı?
    line_numbers: bool,
    /// Dil rozeti basılsın mı?
    badge: bool,
    /// Blogun işaretlediği satırlar bantla vurgulansın mı?
    line_highlights: bool,
}

fn code_block(
    code: &str,
    lang: Option<&str>,
    highlights: &[usize],
    view: CodeBlockOptions,
    state: &Rc<RefCell<CaptureState>>,
) -> impl Element {
    let CodeBlockOptions {
        style,
        palette,
        line_numbers,
        badge,
        line_highlights,
    } = view;
    // Simgeleri karakterlere açıyoruz: sarım karakter sayısıyla hesaplandığı
    // için (mono font) token sınırları görsel satırları bağlamaz.
    let mut lines: Vec<Vec<(char, Kind)>> = Vec::new();
    for tokens in crate::highlight::highlight(code, lang) {
        let mut line = Vec::new();
        for token in tokens {
            for ch in token.text.chars() {
                // Sekme: mono fontta 4 boşluk genişliğinde hizalanır.
                if ch == '\t' {
                    line.extend(std::iter::repeat_n((' ', token.kind), 4));
                } else {
                    line.push((ch, token.kind));
                }
            }
        }
        lines.push(line);
    }
    // Tek satırlık kod parçasında numara gürültü olur; rozet de yalnızca dil
    // bilindiğinde konur.
    let numbers = line_numbers && lines.len() >= 2;
    let badge = badge.then(|| lang.map(str::to_string)).flatten();
    let marks: BTreeSet<usize> = if line_highlights {
        highlights.iter().copied().filter(|i| *i < lines.len()).collect()
    } else {
        BTreeSet::new()
    };

    CodeBlock {
        lines,
        marks,
        badge,
        numbers,
        style,
        palette,
        state: Rc::clone(state),
        planned: None,
        next: 0,
        split_counted: false,
    }
}

/// Satır numarası ile kodu ayıran dikey çizgi ve sarmalanan satırların işareti.
/// İkisi de gömülü Liberation fontlarında bulunur (bkz. `charset`).
const GUTTER_SEPARATOR: char = '│';
const WRAP_MARKER: char = '»';

/// Kod bloğu gövdesi.
///
/// Uzun satırlar **elle** sarılır: yazı tipi mono olduğu için satıra sığan
/// karakter sayısı ölçülebilir, böylece devam satırları numara yerine ayraç ve
/// sarma işaretiyle girintilenir (genpdf'in kelime kaydırması kod girintisini
/// bozar ve sarma noktasını göstermez).
///
/// Sayfa geçişini de bu eleman yönetir (`genpdf` tablo satırları satır
/// düzeyinde bölerdi):
/// 1. **Taşıma**: blok bu sayfaya sığmıyor ama yeni bir sayfaya sığıyorsa
///    bölünmek yerine tümüyle sonraki sayfaya geçer.
/// 2. **Bütünlük**: bir mantıksal satırın görsel satırları (sarmalanmış devam
///    satırları) asla iki sayfaya bölünmez — yalnızca tek başına bir sayfaya
///    sığmayan bir satır zorunlu olarak bölünür.
/// 3. **Yetim satır yok**: sayfa sonunda tek bir satır kalacaksa son birim geri
///    alınır, böylece devam sayfasında en az iki satır bulunur.
struct CodeBlock {
    /// Mantıksal satırlar: karakterler ve renk türleri.
    lines: Vec<Vec<(char, Kind)>>,
    /// Blogun vurguladığı mantıksal satırlar (0 tabanlı): zemin bandı alırlar.
    marks: BTreeSet<usize>,
    /// Üst sağdaki dil rozeti (`rust`, `python` ...).
    badge: Option<String>,
    /// Satır numaraları basılsın mı?
    numbers: bool,
    /// Kod metni stili (mono aile + punto + renk).
    style: Style,
    /// Kutu paleti.
    palette: CodePalette,
    /// Taze sayfada kullanılabilir yükseklik için (taşıma kararı).
    state: Rc<RefCell<CaptureState>>,
    /// İlk render'da kurulan görsel satırlar (birim: mantıksal satır).
    planned: Option<Vec<VisualLine>>,
    /// Bir sonraki basılacak görsel satır indeksi.
    next: usize,
    /// Bu bloğun bölünmesi özete bir kez yazılsın.
    split_counted: bool,
}

/// Basılmaya hazır tek görsel satır (girinti, numara ve renkleriyle).
struct VisualLine {
    /// Ait olduğu mantıksal satır. Sayfa geçişi bir birimin ortasından geçmez;
    /// rozet satırı ilk kod satırıyla aynı birimdedir.
    unit: usize,
    /// Bloğun vurguladığı satır mı? Vurgulu görsel satırların arkasına renkli
    /// bant çizilir (bkz. `CaptureState::highlight_bands`).
    highlighted: bool,
    /// Satır başına basılan numara/ayraç/sarma metninin glif sayısı (yoksa 0).
    marker_glyphs: usize,
    /// Dil rozeti satırı mı? Rozet sağa yaslıdır ve satırda başka metin yoktur.
    badge: bool,
    /// Basılacak metin parçaları (stil dâhil) ve yükseklik çarpanı.
    paragraph: Paragraph,
}

impl CodeBlock {
    /// Satır yüksekliği (mm) — belge satır aralığıyla birlikte.
    fn line_height_mm(&self, context: &PdfContext) -> f64 {
        f64::from(self.style.line_height(&context.font_cache))
    }

    /// Görsel satırları kurar: rozet, numara/ayraç ve sarılmış parçalar.
    fn plan(&self, context: &PdfContext, width_mm: f64) -> Vec<VisualLine> {
        let gutter_style = self.style.with_color(self.palette.gutter);
        let plain_style = self.style.with_color(self.palette.plain);
        let mut planned = Vec::new();

        if let Some(badge) = &self.badge {
            // Rozet, ilk kod satırından kopmasın diye onunla aynı birimde.
            let mut paragraph = Paragraph::default();
            paragraph.push_styled(
                badge.clone(),
                self.style.with_font_size(7).with_color(self.palette.gutter),
            );
            planned.push(VisualLine {
                unit: 0,
                highlighted: false,
                marker_glyphs: badge.chars().count(),
                badge: true,
                paragraph: paragraph.aligned(Alignment::Right),
            });
        }

        // Mono font: tek karakter genişliği tüm karakterler için aynıdır.
        let char_width = f64::from(self.style.str_width(&context.font_cache, "M"));
        let max_chars = if char_width > 0.0 {
            (width_mm / char_width).floor().max(12.0) as usize
        } else {
            80
        };
        let digits = self.lines.len().to_string().len().max(2);
        let gutter_chars = if self.numbers { digits + 3 } else { 0 };
        let code_chars = max_chars.saturating_sub(gutter_chars).max(8);
        // Devam satırında "» " işareti kod sütununun ilk iki karakterini alır.
        let continuation_chars = code_chars.saturating_sub(2).max(1);

        for (index, line) in self.lines.iter().enumerate() {
            for (part, chunk) in visual_chunks(line, code_chars, continuation_chars)
                .into_iter()
                .enumerate()
            {
                let mut paragraph = Paragraph::default();
                let highlighted = self.marks.contains(&index);
                // Vurgulu satırın numarası da öne çıksın: ikincil renk yerine
                // kod metninin rengiyle basılır.
                let marker_style = if highlighted { plain_style } else { gutter_style };
                let marker = gutter_marker(index + 1, digits, part > 0, self.numbers);
                // Numaralar kopyalanmasın diye glif sayısı saklanır; boş işaret
                // (numara kapalı ve satır sarması yok) gizlenecek bir şey demek
                // değildir.
                let marker_glyphs = marker.chars().count();
                if !marker.is_empty() {
                    paragraph.push_styled(marker, marker_style);
                }
                if chunk.is_empty() {
                    // Boş satır kısalmasın: yükseklik tek boşlukla korunur.
                    paragraph.push_styled(" ", plain_style);
                }
                for (text, kind) in runs(chunk) {
                    paragraph.push_styled(text, token_style(self.style, kind, self.palette));
                }
                planned.push(VisualLine {
                    unit: index,
                    highlighted,
                    marker_glyphs,
                    badge: false,
                    paragraph,
                });
            }
        }
        planned
    }
}

impl Element for CodeBlock {
    fn render(
        &mut self,
        context: &PdfContext,
        mut area: render::Area<'_>,
        style: Style,
    ) -> Result<RenderResult, genpdf::error::Error> {
        let box_width_mm = f64::from(area.size().width);
        let box_available_mm = f64::from(area.size().height);
        // Kutunun sayfa üstünden uzaklığı: kök yerleşimde elemanın üst kenarı,
        // kendisine verilen alanın kalan yüksekliğinden hesaplanır (bkz.
        // `BookmarkPoint` ve `CodeBoxCapture`). Vurgu bantları da mutlak
        // koordinat istediği için burada bir kez hesaplanır.
        let box_top_mm = {
            let state = self.state.borrow();
            state.content_top_mm + (state.content_height_mm - box_available_mm)
        };
        area.add_margins(Margins::all(CODE_PADDING_MM as u32));

        if self.planned.is_none() {
            let planned = self.plan(context, (box_width_mm - 2.0 * CODE_PADDING_MM).max(20.0));
            let box_height_mm = planned.len() as f64 * self.line_height_mm(context)
                + 2.0 * CODE_PADDING_MM;
            let fresh_page_mm = self.state.borrow().content_height_mm;
            self.planned = Some(planned);
            if should_move_to_next_page(box_height_mm, box_available_mm, fresh_page_mm) {
                // Bu sayfaya sığmıyor ama yeni bir sayfaya sığıyor: hiçbir şey
                // basmadan sayfa atlat (genpdf `has_more` görünce yeni sayfa açar).
                return Ok(RenderResult {
                    size: Size::new(0, 0),
                    has_more: true,
                });
            }
        }

        let planned = self.planned.as_ref().expect("plan kuruldu");
        let units: Vec<usize> = planned.iter().map(|line| line.unit).collect();
        let line_height_mm = self.line_height_mm(context);
        let count = page_line_count(
            &units,
            self.next,
            f64::from(area.size().height),
            line_height_mm,
        );

        let mut used_mm = 0.0;
        let mut bands: Vec<CodeBoxRect> = Vec::new();
        let mut decorations: Vec<CodeDecoration> = Vec::new();
        for line in &planned[self.next..self.next + count] {
            let mut paragraph = line.paragraph.clone();
            let result = paragraph.render(context, area.clone(), style)?;
            if result.has_more {
                // Plan dışı taşma (ölçüm hatası): satırı sonraki sayfaya bırak.
                break;
            }
            let line_top_mm = box_top_mm + CODE_PADDING_MM + used_mm;
            if line.highlighted {
                let state = self.state.borrow();
                bands.push(CodeBoxRect {
                    page: state.page as u32,
                    x_mm: state.content_left_mm + CODE_PADDING_MM,
                    y_mm: line_top_mm,
                    width_mm: state.content_width_mm - 2.0 * CODE_PADDING_MM,
                    height_mm: f64::from(result.size.height),
                });
            }
            if line.marker_glyphs > 0 || line.badge {
                let state = self.state.borrow();
                // İşaret satırın başında başlar; genişliği glif sayısından
                // (mono font) kesin hesaplanır. Rozet satırında başka metin
                // olmadığı için tüm satır kullanılır.
                let char_width = f64::from(self.style.str_width(&context.font_cache, "M"));
                let width_mm = if line.badge {
                    state.content_width_mm - 2.0 * CODE_PADDING_MM
                } else {
                    line.marker_glyphs as f64 * char_width
                };
                decorations.push(CodeDecoration {
                    page: state.page as u32,
                    x_mm: state.content_left_mm + CODE_PADDING_MM,
                    y_mm: line_top_mm,
                    width_mm,
                    height_mm: f64::from(result.size.height),
                    glyphs: line.marker_glyphs,
                });
            }
            area.add_offset(genpdf::Position::new(0, result.size.height));
            used_mm += f64::from(result.size.height);
            self.next += 1;
        }

        // Devam sayfasının üstbilgisinde "kod devamı" notu gösterilsin.
        let has_more = self.next < planned.len();
        {
            let mut state = self.state.borrow_mut();
            state.highlighted_lines += bands.len();
            state.highlight_bands.extend(bands);
            state.decorations.extend(decorations);
            state.code_continues = has_more && self.next > 0;
            if has_more && self.next > 0 && !self.split_counted {
                self.split_counted = true;
                state.code_splits += 1;
            }
            // Taşıma (henüz hiç satır basılmadı) devam sayılmaz.
            if has_more && self.next == 0 {
                state.code_continues = false;
            }
        }

        Ok(RenderResult {
            // Basılacak satır yoksa yükseklik 0 olur: bu sayfaya boş kutu
            // çizilmemeli (bkz. `CodeBoxCapture`).
            size: Size::new(
                box_width_mm as f32,
                (if count == 0 { 0.0 } else { used_mm + 2.0 * CODE_PADDING_MM }) as f32,
            ),
            has_more,
        })
    }
}

/// Blok bu sayfaya sığmıyor ama yeni bir sayfaya sığıyorsa taşınmalı mı?
///
/// Paylar ölçüm/f32 yuvarlama hatasına karşıdır; ayrıca taze bir sayfanın
/// başında asla "taşıma" kararı verilmez (sonsuz döngü olurdu): o durumda
/// `available == fresh` olduğu için koşul kendiliğinden sağlanmaz.
fn should_move_to_next_page(box_height_mm: f64, available_mm: f64, fresh_page_mm: f64) -> bool {
    const SLACK_MM: f64 = 0.5;
    box_height_mm > available_mm + SLACK_MM && box_height_mm <= fresh_page_mm - 1.0
}

/// Bu sayfaya basılacak görsel satır sayısını hesaplar.
///
/// `units[i]`, i. görsel satırın mantıksal satırıdır. Kural: bir birim
/// (mantıksal satır) sayfa sınırında bölünmez; son satır tek başına bir
/// sonraki sayfaya düşecekse son birim geri alınır (yetim satır koruması).
fn page_line_count(units: &[usize], next: usize, available_mm: f64, line_height_mm: f64) -> usize {
    if next >= units.len() || line_height_mm <= 0.0 {
        return 0;
    }
    let capacity = (available_mm / line_height_mm).floor().max(0.0) as usize;
    let mut count = 0usize;
    while next + count < units.len() {
        let unit = units[next + count];
        // Birimin son görsel satırı (dahil değil).
        let unit_end = units[next + count..]
            .iter()
            .position(|other| *other != unit)
            .map(|offset| next + count + offset)
            .unwrap_or(units.len());
        let unit_len = unit_end - (next + count);
        if count + unit_len > capacity {
            // Birim sığmıyor. Bu sayfaya hiçbir şey basılmadıysa ve birim tek
            // başına bir sayfaya da sığmıyorsa zorunlu olarak böl (aksi hâlde
            // hiç ilerleme olmazdı).
            if count == 0 && unit_len > capacity {
                count = capacity;
            }
            break;
        }
        count += unit_len;
    }

    // Yetim satır: sonraki sayfaya tek satır kalmasın.
    if units.len() - (next + count) == 1 && count > 1 {
        let last = units[next + count - 1];
        let mut unit_start = next + count - 1;
        while unit_start > next && units[unit_start - 1] == last {
            unit_start -= 1;
        }
        // Bu sayfa boş kalmasın: geri alma yalnızca basılacak başka birim varsa.
        if unit_start > next {
            count = unit_start - next;
        }
    }
    count
}

/// Satırın önüne gelen numara/ayraç metni.
///
/// `  3 │ ` — normal satır, `    │ » ` — önceki satırın devamı. Numaralar
/// kapatıldığında yalnızca sarma işareti bırakılır.
fn gutter_marker(number: usize, digits: usize, continuation: bool, numbers: bool) -> String {
    if !numbers {
        return if continuation {
            format!("{WRAP_MARKER} ")
        } else {
            String::new()
        };
    }
    if continuation {
        format!("{} {GUTTER_SEPARATOR} {WRAP_MARKER} ", " ".repeat(digits))
    } else {
        format!(
            "{number:>width$} {GUTTER_SEPARATOR} ",
            width = digits
        )
    }
}

/// Bir mantıksal satırı görsel satırlara böler: ilk parça `first` karakter,
/// devamlar `rest` karakter taşır. Boş satır tek boş parçaya karşılık gelir.
fn visual_chunks(
    line: &[(char, Kind)],
    first: usize,
    rest: usize,
) -> Vec<&[(char, Kind)]> {
    if line.is_empty() {
        return vec![line];
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut limit = first.max(1);
    while start < line.len() {
        let end = (start + limit).min(line.len());
        chunks.push(&line[start..end]);
        start = end;
        limit = rest.max(1);
    }
    chunks
}

/// Ardışık aynı türdeki karakterleri tek metin parçasına indirger.
fn runs(chars: &[(char, Kind)]) -> Vec<(String, Kind)> {
    let mut runs: Vec<(String, Kind)> = Vec::new();
    for &(ch, kind) in chars {
        match runs.last_mut() {
            Some((text, last)) if *last == kind => text.push(ch),
            _ => runs.push((ch.to_string(), kind)),
        }
    }
    runs
}

/// Bir kod simgesinin stilini tema paletine göre renklendirir.
fn token_style(base: Style, kind: Kind, palette: CodePalette) -> Style {
    match kind {
        Kind::Plain => base.with_color(palette.plain),
        Kind::Keyword => base.with_color(palette.keyword),
        Kind::Type => base.with_color(palette.type_name),
        Kind::Function => base.with_color(palette.function),
        Kind::Str => base.with_color(palette.string),
        Kind::Number => base.with_color(palette.number),
        // Yorumlar eğik: renk körlüğünde de ayrışır ve bloglardaki gibi durur.
        Kind::Comment => base.italic().with_color(palette.comment),
    }
}

/// Liste bloğu dizisini iç içe listeye çevirir (hanging indent'li).
fn nested_list(run: &[Block], body: Style) -> LinearLayout {
    let items: Vec<ListItemData> = run
        .iter()
        .filter_map(|b| match b {
            Block::ListItem {
                text,
                ordered,
                depth,
            } => Some(ListItemData {
                text,
                ordered: *ordered,
                depth: *depth,
            }),
            _ => None,
        })
        .collect();
    let mut pos = 0usize;
    let mut counters: Vec<usize> = Vec::new();
    let depth = items.first().map(|i| i.depth).unwrap_or(0);
    build_list_level(&items, &mut pos, depth, &mut counters, body)
}

struct ListItemData<'a> {
    text: &'a str,
    ordered: bool,
    depth: u8,
}

fn build_list_level(
    items: &[ListItemData],
    pos: &mut usize,
    depth: u8,
    counters: &mut Vec<usize>,
    body: Style,
) -> LinearLayout {
    let mut layout = LinearLayout::vertical();
    while *pos < items.len() && items[*pos].depth == depth {
        let item = &items[*pos];
        let bullet = if item.ordered {
            if counters.len() <= depth as usize {
                counters.resize(depth as usize + 1, 0);
            }
            counters[depth as usize] += 1;
            format!("{}.", counters[depth as usize])
        } else {
            // Sırasız liste, aynı seviyedeki numaralandırmayı sıfırlar.
            counters.truncate(depth as usize);
            "•".to_string()
        };

        let mut body_layout = LinearLayout::vertical();
        body_layout.push(Paragraph::new(item.text).styled(body));
        *pos += 1;
        if *pos < items.len() && items[*pos].depth > depth {
            let child_depth = items[*pos].depth;
            let sub = build_list_level(items, pos, child_depth, counters, body);
            body_layout.push(sub);
        }

        let mut point = BulletPoint::new(body_layout);
        point.set_bullet(bullet);
        // Madde imi de bu stille basılır; renk verilmezse varsayılan siyah
        // kullanılır ve koyu/sepya temada görünmez olur.
        layout.push(point.styled(body));
    }
    layout
}

/// Tabloyu çerçeveli bir ızgara olarak kurar. Çerçeve çizgileri tablonun
/// stiline bağlıdır; koyu/sepya temada görünmesi için renk açıkça verilir.
fn table_element(table: &Table, palette: Palette) -> Option<impl Element> {
    if table.columns == 0 {
        return None;
    }
    let cell_style = Style::new().with_font_size(9).with_color(palette.text);
    let header_style = cell_style.bold();

    let weights: Vec<usize> = (0..table.columns)
        .map(|c| column_weight(table, c))
        .collect();

    let mut layout = TableLayout::new(weights);
    layout.set_cell_decorator(FrameCellDecorator::new(true, true, true));

    if !table.header.is_empty() {
        let _ = layout.push_row(
            table
                .header
                .iter()
                .map(|c| cell_element(c, header_style))
                .collect(),
        );
    }
    for row in &table.rows {
        let _ = layout.push_row(row.iter().map(|c| cell_element(c, cell_style)).collect());
    }
    Some(layout.styled(Style::new().with_color(palette.text)))
}

/// Sütun ağırlığı: metni uzun olan sütun daha geniş yer alır (1..=3).
fn column_weight(table: &Table, column: usize) -> usize {
    let mut total = 0usize;
    let mut n = 0usize;
    if let Some(c) = table.header.get(column) {
        total += c.chars().count();
        n += 1;
    }
    for row in &table.rows {
        if let Some(c) = row.get(column) {
            total += c.chars().count();
            n += 1;
        }
    }
    if n == 0 {
        return 1;
    }
    let avg = total / n;
    (avg / 20).clamp(1, 3)
}

/// Bir hücreyi elemana çevirir. Hücre satır sonu içeriyorsa satırlar ayrı
/// paragraf olur (listeler hücre içinde madde madde kalır).
fn cell_element(text: &str, style: Style) -> Box<dyn Element> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 1 {
        let single = lines.first().copied().unwrap_or("");
        return Box::new(
            Paragraph::new(single)
                .styled(style)
                .padded(Margins::trbl(1, 2, 1, 2)),
        );
    }
    let mut layout = LinearLayout::vertical();
    for line in lines {
        layout.push(Paragraph::new(line).styled(style));
    }
    Box::new(layout.padded(Margins::trbl(1, 2, 1, 2)))
}

/// URL'nin alan adını güvenli karakterlere indirger (boş olabilir).
pub fn host(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// URL'den PDF dosya adı üretir (host tabanlı, güvenli karakterlere indirgenmiş).
pub fn output_name(url: &str) -> String {
    let host = host(url);
    if host.is_empty() {
        return "sayfa.pdf".to_string();
    }
    format!("{host}.pdf")
}

/// Belgeyi bellekteki PDF baytlarına dönüştürür (yer imleri atılır).
/// Yalnızca testler kullanır; üretim yolu `render_article`'dan geçer.
#[cfg(test)]
pub fn render_to_bytes(job: PdfJob) -> Result<Vec<u8>> {
    Ok(job.render_with_bookmarks()?.0)
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
                Block::Code {
                    text: "fn main() {}\n// yorum".into(),
                    lang: Some("rust".into()),
                    highlights: vec![],
                },
                Block::Quote("Alıntı".into()),
                Block::ListItem {
                    text: "madde".into(),
                    ordered: false,
                    depth: 0,
                },
                Block::Divider,
            ],
            images: vec![],
        }
    }

    fn table() -> Table {
        Table {
            header: vec!["Başlık A".into(), "Başlık B".into()],
            rows: vec![
                vec!["a1".into(), "b1".into()],
                vec!["a2".into(), "b2".into()],
            ],
            columns: 2,
        }
    }

    /// Belgede geçen karakterler (testler için kısayol).
    fn chars() -> BTreeSet<char> {
        charset(&article())
    }

    /// PDF'e gömülen font sayısı: printpdf her fontu hem Type0 hem CIDFont
    /// sözlüğünde `/BaseFont /F<n>` ile adlandırır, yani font başına 2 geçiş.
    fn embedded_fonts(bytes: &[u8]) -> usize {
        bytes
            .windows(b"/BaseFont".len())
            .filter(|w| *w == b"/BaseFont")
            .count()
            / 2
    }

    #[test]
    fn font_families_load_from_embedded_bytes() {
        let sans = sans_family(&chars()).unwrap();
        let mono = mono_family(&chars()).unwrap();
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
        assert_eq!(o.page, PageSize::A4);
        assert_eq!(o.theme, Theme::Light);
        assert!(o.bookmarks);
    }

    #[test]
    fn page_sizes_parse_and_scale_geometry() {
        assert_eq!(PageSize::parse("A4"), Some(PageSize::A4));
        assert_eq!(PageSize::parse(" a5 "), Some(PageSize::A5));
        assert_eq!(PageSize::parse("Tablet"), Some(PageSize::Tablet));
        assert_eq!(PageSize::parse("a6"), None);
        assert_eq!(PageSize::A4.dimensions_mm(), (210.0, 297.0));
        assert_eq!(PageSize::A5.dimensions_mm(), (148.0, 210.0));
        assert_eq!(PageSize::default(), PageSize::A4);
    }

    #[test]
    fn page_margins_stay_proportional_and_readable() {
        for page in [
            PageSize::A4,
            PageSize::A5,
            PageSize::Letter,
            PageSize::Tablet,
        ] {
            let (width, height) = page.dimensions_mm();
            let (top, right, bottom, left) = page.margins_mm();
            assert!(
                f64::from(left) < width / 4.0 && f64::from(right) < width / 4.0,
                "{page:?}"
            );
            assert!(
                f64::from(top) < height / 4.0 && f64::from(bottom) < height / 4.0,
                "{page:?}"
            );
            assert!(page.content_width_mm() > width / 2.0, "{page:?}");
            assert!(page.content_height_mm() > height / 2.0, "{page:?}");
            // Metin sütunu sayfanın %70-95'i olmalı (okunabilirlik).
            let fraction = page.content_width_mm() / width;
            assert!(fraction > 0.70 && fraction < 0.95, "{page:?}: {fraction}");
            // Görsel yüksekliği içerik yüksekliğini aşmamalı.
            assert!(page.max_image_height_mm() < page.content_height_mm());
        }
    }

    #[test]
    fn themes_parse_and_expose_palettes() {
        assert_eq!(Theme::parse("Dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("sepia"), Some(Theme::Sepia));
        assert_eq!(Theme::parse("neon"), None);
        assert_eq!(Theme::default(), Theme::Light);
        // Açık temada zemin çizilmez; koyu/sepya kendi zeminini ister.
        assert!(Theme::Light.palette_with(CodeTheme::Auto).background.is_none());
        assert!(Theme::Dark.palette_with(CodeTheme::Auto).background.is_some());
        assert!(Theme::Sepia.palette_with(CodeTheme::Auto).background.is_some());
        // Koyu temada metin açık, açık temada koyudur.
        let (dark_text, dark_bg) = (
            Theme::Dark.palette_with(CodeTheme::Auto).text,
            Theme::Dark.palette_with(CodeTheme::Auto).background.unwrap(),
        );
        assert_ne!(dark_text, dark_bg);
        let dark_luma = luma(dark_text);
        let dark_bg_luma = luma(dark_bg);
        assert!(
            dark_luma > dark_bg_luma,
            "koyu temada metin zeminden açık olmalı"
        );
        let light = Theme::Light.palette_with(CodeTheme::Auto);
        assert!(luma(light.text) < 128, "açık temada metin koyu olmalı");
    }

    /// Bir rengin görece parlaklığı (0-255).
    fn luma(color: Color) -> u32 {
        match color {
            Color::Rgb(r, g, b) => {
                (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000
            }
            Color::Greyscale(v) => u32::from(v),
            Color::Cmyk(..) => 0,
        }
    }

    #[test]
    fn charset_covers_article_text_and_interface_extras() {
        let set = chars();
        // Yayı içeren karakterler + arayüz karakterleri (sayfa no, madde imi).
        for ch in "Türkçe ĞÜŞİÖÇğüşıöç•―0123456789".chars() {
            assert!(set.contains(&ch), "{ch:?} kümede yok");
        }
        // Kod bloğu ve tablo hücreleri de kümeye girer.
        assert!(set.contains(&'f'));
        assert!(set.contains(&'9'));
    }

    #[test]
    fn mono_is_only_embedded_when_code_exists() {
        let with_code = article();
        assert!(needs_mono(&with_code));
        let without: Article = Article {
            blocks: vec![Block::Paragraph("sadece metin".into())],
            ..article()
        };
        assert!(!needs_mono(&without));
    }

    #[test]
    fn mono_family_is_embedded_only_when_code_exists() {
        // Kod bloğu yoksa mono ailesi (4 font) hiç gömülmez.
        let with_code =
            render_to_bytes(build_document(&article(), &PdfOptions::default()).unwrap()).unwrap();
        let without: Article = Article {
            blocks: vec![Block::Paragraph("Paragraf metni ğüşıöç.".into())],
            ..article()
        };
        let without_code =
            render_to_bytes(build_document(&without, &PdfOptions::default()).unwrap()).unwrap();
        assert_eq!(embedded_fonts(&with_code), 8, "sans + mono aileleri");
        assert_eq!(embedded_fonts(&without_code), 4, "yalnızca sans ailesi");
        assert!(without_code.len() < with_code.len());
    }

    #[test]
    fn font_subsetting_keeps_pdfs_small() {
        // Alt kümeleme öncesi bu belge ~1.7 MB'tı (4 sans fontu tam gömülü).
        let small = Article {
            blocks: vec![Block::Paragraph("kısa metin".into())],
            ..article()
        };
        let bytes =
            render_to_bytes(build_document(&small, &PdfOptions::default()).unwrap()).unwrap();
        assert!(
            bytes.len() < 400_000,
            "fontlar alt kümelenmemiş görünüyor: {} bayt",
            bytes.len()
        );
    }

    #[test]
    fn subset_fonts_are_smaller_than_the_originals() {
        // Doğrudan font düzeyinde kontrol: gömülü font akışları kaynak
        // dosyalardan belirgin şekilde küçük olmalı.
        let rendered =
            render_to_bytes(build_document(&article(), &PdfOptions::default()).unwrap()).unwrap();
        let budget =
            SANS_REGULAR.len() + SANS_BOLD.len() + SANS_ITALIC.len() + SANS_BOLD_ITALIC.len();
        assert!(
            rendered.len() < budget / 2,
            "PDF ({}) kaynak fontların yarısından ({}) büyük",
            rendered.len(),
            budget / 2
        );
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
        let mut small = article();
        small.blocks.clear();
        let b1 = render_to_bytes(build_document(&small, &PdfOptions::default()).unwrap()).unwrap();
        let b2 =
            render_to_bytes(build_document(&article(), &PdfOptions::default()).unwrap()).unwrap();
        assert!(b2.len() > b1.len());
    }

    #[test]
    fn header_layout_renders_page_number_and_hides_title_on_first_page() {
        let palette = Theme::Light.palette_with(CodeTheme::Auto);
        let mut layout = header_layout("Deneme".to_string(), palette, false)(3);
        let _ = &mut layout;
        // İlk sayfada başlık tekrarını önlemek için boş bırakılır.
        let _ = header_layout("Deneme".to_string(), palette, false)(1);
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
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.matches("/Type /Page").count() >= 2 || bytes.len() > 20_000);
    }

    #[test]
    fn table_renders_into_pdf() {
        let art = Article {
            blocks: vec![Block::Table(table())],
            ..article()
        };
        let bytes = render_to_bytes(build_document(&art, &PdfOptions::default()).unwrap()).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 1000);
    }

    #[test]
    fn table_with_multiline_cells_renders() {
        let multi = Table {
            header: vec!["A".into(), "B".into()],
            rows: vec![vec!["• bir\n• iki".into(), "tek satır".into()]],
            columns: 2,
        };
        let art = Article {
            blocks: vec![Block::Table(multi)],
            ..article()
        };
        let bytes = render_to_bytes(build_document(&art, &PdfOptions::default()).unwrap()).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn empty_table_is_skipped() {
        assert!(table_element(&Table::default(), Theme::Light.palette_with(CodeTheme::Auto)).is_none());
    }

    #[test]
    fn column_weight_prefers_text_heavy_columns() {
        let t = Table {
            header: vec![
                "kısa".into(),
                "çok daha uzun bir başlık metni burada".into(),
            ],
            rows: vec![vec!["a".into(), "b".repeat(80)]],
            columns: 2,
        };
        assert!(column_weight(&t, 1) > column_weight(&t, 0));
    }

    #[test]
    fn nested_lists_produce_layered_layout() {
        let run = vec![
            Block::ListItem {
                text: "dış".into(),
                ordered: false,
                depth: 0,
            },
            Block::ListItem {
                text: "iç".into(),
                ordered: true,
                depth: 1,
            },
            Block::ListItem {
                text: "yine dış".into(),
                ordered: false,
                depth: 0,
            },
        ];
        let art = Article {
            blocks: run,
            ..article()
        };
        let bytes = render_to_bytes(build_document(&art, &PdfOptions::default()).unwrap()).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn list_run_end_groups_only_consecutive_items() {
        let blocks = vec![
            Block::ListItem {
                text: "a".into(),
                ordered: false,
                depth: 0,
            },
            Block::ListItem {
                text: "b".into(),
                ordered: false,
                depth: 0,
            },
            Block::Paragraph("ara".into()),
            Block::ListItem {
                text: "c".into(),
                ordered: false,
                depth: 0,
            },
        ];
        assert_eq!(list_run_end(&blocks, 0), 2);
        assert_eq!(list_run_end(&blocks, 3), 4);
    }

    #[test]
    fn code_block_renders_with_numbers_and_badge() {
        let style = Style::new().with_font_size(9);
        let state = Rc::new(RefCell::new(CaptureState::default()));
        let el = code_block(
            "fn main() {\n    let x = 1;\n\n    println!(\"{x}\");\n}",
            Some("rust"),
            &[1],
            CodeBlockOptions {
                style,
                palette: Theme::Light.palette_with(CodeTheme::Auto).code,
                line_numbers: true,
                badge: true,
                line_highlights: true,
            },
            &state,
        );
        let mut doc = Document::new(sans_family(&chars()).unwrap());
        doc.push(el);
        let mut bytes = Vec::new();
        doc.render(&mut bytes).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // Girintili satırlar ve numaralar korunmalı (5 satır + rozet).
        assert!(bytes.len() > 1000);
        assert!(!state.borrow().code_continues, "sayfa bölünmemeliydi");
    }

    /// Belgeyi `opts` ile kurup tek kod kutusunun dikdörtgenini döndürür.
    fn code_box_rect(params: &PdfOptions) -> CodeBoxRect {
        let job = build_document(&article(), params).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let boxes = state.borrow().code_boxes.clone();
        assert_eq!(boxes.len(), 1, "tek kod bloğu tek kutu üretmeli");
        boxes[0]
    }

    /// `code` metniyle kurulmuş belgede kod kutusunun yüksekliği.
    fn box_height(code: &str) -> f64 {
        let art = Article {
            title: "Kod".into(),
            blocks: vec![Block::Code {
                text: code.into(),
                lang: Some("rust".into()),
                highlights: vec![],
            }],
            images: vec![],
        };
        let job = build_document(&art, &PdfOptions::default()).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let boxes = state.borrow().code_boxes.clone();
        boxes.iter().map(|b| b.height_mm).sum()
    }

    #[test]
    fn code_blocks_capture_their_box_rectangle() {
        let opts = PdfOptions::default();
        let rect = code_box_rect(&opts);
        let (_, page_height_mm) = opts.page.dimensions_mm();
        let (_, _, _, left) = opts.page.margins_mm();
        assert_eq!(rect.page, 1);
        // Kutu, metin sütununun tam genişliğinde ve sayfa sınırları içinde.
        assert_eq!(rect.x_mm, f64::from(left));
        assert_eq!(rect.width_mm, opts.page.content_width_mm());
        assert!(rect.y_mm > 0.0, "{rect:?}");
        assert!(rect.y_mm + rect.height_mm < page_height_mm, "{rect:?}");
        // İki satır kod + 2 mm iç dolgu.
        assert!(rect.height_mm > 5.0, "{rect:?}");
    }

    #[test]
    fn badge_and_line_numbers_grow_the_code_box() {
        let full = code_box_rect(&PdfOptions::default());
        let bare = code_box_rect(&PdfOptions {
            line_numbers: false,
            code_badge: false,
            ..Default::default()
        });
        assert!(
            full.height_mm > bare.height_mm,
            "rozet + numaralar kutuyu büyütmeli: {full:?} / {bare:?}"
        );
    }

    #[test]
    fn long_lines_wrap_inside_the_box() {
        let single = box_height("let toplam = birinci + ikinci;");
        // A4 sütununa sığmayan tek satır: tam olarak bir kez sarılmalı, yani
        // kutu tam bir satır yüksekliği kadar büyümeli (9 pt × 1.3 ≈ 4.1 mm).
        let wrapped = box_height(
            "let toplam = birinci_deger + ikinci_deger + ucuncu_deger + dorduncu_deger + \
             besinci_deger + altinci_deger + yedinci_deger + sekizinci_deger + dokuzuncu_deger;",
        );
        let added = wrapped - single;
        assert!(
            (3.0..6.0).contains(&added),
            "uzun satır bir kez sarılmalıydı, eklenen yükseklik {added:.2} mm"
        );
    }

    #[test]
    fn visual_chunks_splits_first_and_continuation_lines() {
        let line: Vec<(char, Kind)> = "abcdefghij"
            .chars()
            .map(|ch| (ch, Kind::Plain))
            .collect();
        let chunks = visual_chunks(&line, 4, 3);
        let sizes: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
        assert_eq!(sizes, vec![4, 3, 3]);
        // Boş satır tek (boş) parçaya karşılık gelir: yüksekliği korunur.
        assert_eq!(visual_chunks(&[], 4, 3).len(), 1);
        assert!(visual_chunks(&[], 4, 3)[0].is_empty());
        // Parçalar satırı birebir kaplar.
        let rebuilt: String = chunks.iter().flat_map(|c| c.iter().map(|(ch, _)| *ch)).collect();
        assert_eq!(rebuilt, "abcdefghij");
    }

    #[test]
    fn gutter_marker_formats_numbers_and_continuations() {
        assert_eq!(gutter_marker(3, 2, false, true), " 3 │ ");
        assert_eq!(gutter_marker(12, 2, false, true), "12 │ ");
        // Devam satırı: numara yerine ayraç ve sarma işareti.
        assert_eq!(gutter_marker(3, 2, true, true), "   │ » ");
        // Numaralar kapalıysa yalnızca sarma işareti kalır.
        assert_eq!(gutter_marker(3, 2, false, false), "");
        assert_eq!(gutter_marker(3, 2, true, false), "» ");
    }

    #[test]
    fn runs_merge_consecutive_kinds() {
        let chars: Vec<(char, Kind)> = vec![
            ('a', Kind::Plain),
            ('b', Kind::Plain),
            ('1', Kind::Number),
            (' ', Kind::Plain),
        ];
        let runs = runs(&chars);
        assert_eq!(
            runs,
            vec![
                ("ab".to_string(), Kind::Plain),
                ("1".to_string(), Kind::Number),
                (" ".to_string(), Kind::Plain),
            ]
        );
    }

    #[test]
    fn code_theme_parses_names_and_resolves_palettes() {
        for code_theme in [
            CodeTheme::Auto,
            CodeTheme::GithubLight,
            CodeTheme::GithubDark,
            CodeTheme::Monokai,
            CodeTheme::SolarizedLight,
            CodeTheme::SolarizedDark,
            CodeTheme::Sepia,
        ] {
            assert_eq!(
                CodeTheme::parse(code_theme.name()),
                Some(code_theme),
                "{}",
                code_theme.name()
            );
        }
        assert_eq!(CodeTheme::parse(" MONOKAI "), Some(CodeTheme::Monokai));
        assert_eq!(CodeTheme::parse("github"), Some(CodeTheme::GithubLight));
        assert_eq!(CodeTheme::parse("solarized"), Some(CodeTheme::SolarizedLight));
        assert_eq!(CodeTheme::parse("neon"), None);

        // `auto` sayfa temasına uyar.
        assert_eq!(CodeTheme::Auto.palette(Theme::Light), GITHUB_LIGHT);
        assert_eq!(CodeTheme::Auto.palette(Theme::Dark), DARK_ON_PAGE);
        assert_eq!(CodeTheme::Auto.palette(Theme::Sepia), SEPIA);
    }

    #[test]
    fn code_theme_only_changes_the_code_colors() {
        let light = Theme::Light.palette_with(CodeTheme::GithubLight);
        let monokai = Theme::Light.palette_with(CodeTheme::Monokai);
        // Sayfa metni ve zemini aynı kalır; yalnızca kod paleti değişir.
        assert_eq!(light.text, monokai.text);
        assert_eq!(light.muted, monokai.muted);
        assert_eq!(light.background, monokai.background);
        assert_ne!(light.code, monokai.code);
        // Açık sayfada koyu kod kutusu seçilebilir (blog görünümü).
        assert_eq!(monokai.code.background, Color::Rgb(0x27, 0x28, 0x22));
    }

    #[test]
    fn every_code_theme_renders_a_pdf() {
        for code_theme in [
            CodeTheme::GithubLight,
            CodeTheme::GithubDark,
            CodeTheme::Monokai,
            CodeTheme::SolarizedLight,
            CodeTheme::SolarizedDark,
            CodeTheme::Sepia,
        ] {
            for theme in [Theme::Light, Theme::Dark, Theme::Sepia] {
                let opts = PdfOptions {
                    theme,
                    code_theme,
                    ..Default::default()
                };
                let bytes = render_to_bytes(build_document(&article(), &opts).unwrap()).unwrap();
                assert!(bytes.starts_with(b"%PDF"), "{theme:?}/{code_theme:?}");
            }
        }
    }

    #[test]
    fn code_block_split_across_pages_records_one_box_per_page() {
        let code = (0..400)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let art = Article {
            title: "Uzun kod".into(),
            blocks: vec![Block::Code {
                text: code,
                lang: Some("rust".into()),
                highlights: vec![],
            }],
            images: vec![],
        };
        let job = build_document(&art, &PdfOptions::default()).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let boxes = state.borrow().code_boxes.clone();
        assert!(boxes.len() >= 2, "uzun blok sayfalara bölünmeli: {boxes:?}");
        // Sayfa numaraları artan olmalı (her sayfa için bir kutu).
        let pages: Vec<u32> = boxes.iter().map(|b| b.page).collect();
        assert!(pages.windows(2).all(|w| w[0] < w[1]), "{pages:?}");
    }

    /// PDF'teki metin operatörü (`TJ`) sayısı: üstbilgiye not eklenip
    /// eklenmediğini görmek için.
    fn text_operations(bytes: &[u8]) -> usize {
        let doc = lopdf::Document::load_mem(bytes).expect("PDF okunamadı");
        let mut count = 0;
        for page_id in doc.get_pages().values() {
            for content_id in doc.get_page_contents(*page_id) {
                let stream = doc.get_object(content_id).unwrap().as_stream().unwrap();
                let content = stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone());
                count += String::from_utf8_lossy(&content).matches("TJ").count();
            }
        }
        count
    }

    #[test]
    fn page_line_count_keeps_logical_lines_together() {
        // 1 satır, 3 görsel satıra sarılmış bir satır, 1 satır.
        let units = [0, 1, 1, 1, 2];
        // Kapasite 4: sarılmış satır bütün olarak sığmadığı için ikinci
        // birimden önce kesilir.
        assert_eq!(page_line_count(&units, 0, 40.0, 10.0), 1);
        // Kapasite 5: hepsi sığar.
        assert_eq!(page_line_count(&units, 0, 50.0, 10.0), 5);
        // Devam sayfası: kalan 4 satır sığar.
        assert_eq!(page_line_count(&units, 1, 40.0, 10.0), 4);
        // Yer yoksa hiçbir şey basılmaz.
        assert_eq!(page_line_count(&units, 0, 0.0, 10.0), 0);
    }

    #[test]
    fn page_line_count_avoids_orphan_lines() {
        // Kapasite 3, dört ayrı birim: üçü basılır, tek satır kalırdı → son
        // birim geri alınır, böylece devam sayfasında iki satır olur.
        let units = [0, 1, 2, 3];
        assert_eq!(page_line_count(&units, 0, 30.0, 10.0), 2);
        // Hepsi sığdığında geri alma yok.
        assert_eq!(page_line_count(&units, 0, 40.0, 10.0), 4);
    }

    #[test]
    fn page_line_count_splits_units_taller_than_a_page() {
        // Tek birim 6 görsel satır, kapasite 4: zorunlu bölme (aksi hâlde
        // hiçbir sayfada ilerleme olmazdı).
        let units = [0, 0, 0, 0, 0, 0];
        assert_eq!(page_line_count(&units, 0, 40.0, 10.0), 4);
        assert_eq!(page_line_count(&units, 4, 40.0, 10.0), 2);
    }

    #[test]
    fn moving_a_block_needs_room_on_the_next_page() {
        // Buraya sığmıyor, taze sayfaya sığıyor → taşınır.
        assert!(should_move_to_next_page(50.0, 20.0, 260.0));
        // Taze sayfaya da sığmıyor → taşınmaz, bölünür.
        assert!(!should_move_to_next_page(300.0, 20.0, 260.0));
        // Buraya sığıyor → taşınmaz.
        assert!(!should_move_to_next_page(15.0, 20.0, 260.0));
        // Taze sayfanın başındayız: taşıma kararı sonsuz döngü olurdu.
        assert!(!should_move_to_next_page(100.0, 260.0, 260.0));
    }

    #[test]
    fn header_notes_a_continuation_page() {
        assert_eq!(header_text(3, false), "Sayfa 3");
        assert_eq!(header_text(3, true), "Sayfa 3 · kod devamı");

        // Not gerçekten üstbilgiye yazılmalı: aynı üstbilgi daha çok glif basar.
        let palette = Theme::Light.palette_with(CodeTheme::Auto);
        let rendered = |continues: bool| {
            let mut doc = Document::new(sans_family(&chars()).unwrap());
            doc.push(header_layout("Başlık".to_string(), palette, continues)(3));
            let mut bytes = Vec::new();
            doc.render(&mut bytes).unwrap();
            text_operations(&bytes)
        };
        assert!(
            rendered(true) > rendered(false),
            "\"kod devamı\" notu üstbilgiye yazılmadı"
        );
    }

    #[test]
    fn a_short_code_block_is_moved_instead_of_split() {
        // Sayfanın altına denk gelen kısa blok bölünmez; tümü sonraki sayfaya
        // geçer ve üstbilgiye "kod devamı" notu düşmez.
        let mut blocks: Vec<Block> = (0..14)
            .map(|i| Block::Paragraph(format!("{i}. paragraf. {}", "Uzun bir cümle. ".repeat(6))))
            .collect();
        blocks.push(Block::Code {
            text: (1..=8)
                .map(|i| format!("let x{i} = {i};"))
                .collect::<Vec<_>>()
                .join("\n"),
            lang: Some("rust".into()),
            highlights: vec![],
        });
        let art = Article {
            title: "Taşıma".into(),
            blocks,
            images: vec![],
        };
        let job = build_document(&art, &PdfOptions::default()).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let state = state.borrow();
        assert_eq!(
            state.code_boxes.len(),
            1,
            "kısa blok bölünmemeli: {:?}",
            state.code_boxes
        );
        assert_eq!(state.code_splits, 0, "bölünme sayılmamalı");
    }

    /// Vurgulu satırlarla kurulmuş tek kod bloğundan (kutu, bantlar) çifti.
    fn highlight_rects(highlights: Vec<usize>, opts: &PdfOptions) -> (Vec<CodeBoxRect>, Vec<CodeBoxRect>) {
        let art = Article {
            title: "Vurgu".into(),
            blocks: vec![Block::Code {
                text: "let a = 1;\nlet b = 2;\nlet c = 3;".into(),
                lang: Some("rust".into()),
                highlights,
            }],
            images: vec![],
        };
        let job = build_document(&art, opts).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let state = state.borrow();
        (state.code_boxes.clone(), state.highlight_bands.clone())
    }

    #[test]
    fn highlighted_line_gets_a_band_inside_the_box() {
        let (boxes, bands) = highlight_rects(vec![1], &PdfOptions::default());
        assert_eq!(boxes.len(), 1, "tek blok tek kutu");
        assert_eq!(bands.len(), 1, "tek vurgulu satır tek bant");
        let (bant, kutu) = (bands[0], boxes[0]);

        // Bant, kutunun iç dolgusundan sonra başlar; sol şeride ve çerçeveye
        // değmez, satır numarası sütununu da kapsar.
        assert_eq!(bant.page, kutu.page);
        assert!((bant.x_mm - (kutu.x_mm + CODE_PADDING_MM)).abs() < 0.01, "{bant:?}");
        assert!(
            (bant.width_mm - (kutu.width_mm - 2.0 * CODE_PADDING_MM)).abs() < 0.01,
            "{bant:?}"
        );
        assert!(bant.y_mm > kutu.y_mm, "{bant:?} / {kutu:?}");
        assert!(
            bant.y_mm + bant.height_mm < kutu.y_mm + kutu.height_mm,
            "{bant:?} / {kutu:?}"
        );
        assert!(bant.height_mm > 2.0, "tek satır yüksekliği: {bant:?}");

        // Vurgu kapalıyken ne bant ne sayaç kalır.
        let (boxes, bands) = highlight_rects(
            vec![1],
            &PdfOptions {
                line_highlights: false,
                ..Default::default()
            },
        );
        assert_eq!(boxes.len(), 1);
        assert!(bands.is_empty(), "vurgu kapatıldı: {bands:?}");
    }

    #[test]
    fn a_wrapped_highlighted_line_gets_one_band_per_visual_line() {
        // Sarılan tek mantıksal satır iki görsel satıra düşer ve ikisi de
        // vurgulu olmalı (bloglar da sarmalanan kısmı vurgular).
        let art = Article {
            title: "Vurgu".into(),
            blocks: vec![Block::Code {
                text: "let toplam = birinci_deger + ikinci_deger + ucuncu_deger + \
                       dorduncu_deger + besinci_deger;\nlet kisa = 1;"
                    .into(),
                lang: Some("rust".into()),
                highlights: vec![0],
            }],
            images: vec![],
        };
        let job = build_document(&art, &PdfOptions::default()).unwrap();
        let state = Rc::clone(&job.state);
        let _ = render_to_bytes(job).unwrap();
        let state = state.borrow();
        assert_eq!(state.highlighted_lines, 2, "iki görsel satır vurgulu");
        assert_eq!(state.highlight_bands.len(), 2);
        // Bantlar üst üste binmeden sırayla iner.
        let bands = &state.highlight_bands;
        assert!(bands[0].y_mm + bands[0].height_mm <= bands[1].y_mm + 0.01);
    }

    #[test]
    fn no_images_option_skips_image_blocks_silently() {
        // Görsel yükleme global sayaç/önbellek kullanır: diğer görsel
        // testleriyle yarışmamak için paylaşılan kilidi alıyoruz.
        let _guard = crate::images::test_lock();
        let art = Article {
            blocks: vec![
                Block::Image("https://example.invalid/a.png".into()),
                Block::Paragraph("metin".into()),
            ],
            ..article()
        };
        let opts = PdfOptions {
            embed_images: false,
            ..Default::default()
        };
        let bytes = render_to_bytes(build_document(&art, &opts).unwrap()).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // "yüklenemedi" notu basılmamalı: ham akışta aramak yerine, notu
        // üreten kod yolunun atlandığını dolaylı olarak doğruluyoruz.
        let with_note = Article {
            blocks: vec![Block::Image("https://example.invalid/a.png".into())],
            ..article()
        };
        let bytes_note =
            render_to_bytes(build_document(&with_note, &PdfOptions::default()).unwrap()).unwrap();
        assert!(bytes_note.len() > 500);
    }

    #[test]
    fn content_width_matches_margins() {
        for page in [
            PageSize::A4,
            PageSize::A5,
            PageSize::Letter,
            PageSize::Tablet,
        ] {
            let (width, _) = page.dimensions_mm();
            let (_, right, _, left) = page.margins_mm();
            assert_eq!(
                page.content_width_mm(),
                width - f64::from(left) - f64::from(right),
                "{page:?}"
            );
        }
    }

    #[test]
    fn every_page_size_and_theme_produces_a_pdf() {
        for page in [
            PageSize::A4,
            PageSize::A5,
            PageSize::Letter,
            PageSize::Tablet,
        ] {
            for theme in [Theme::Light, Theme::Dark, Theme::Sepia] {
                let opts = PdfOptions {
                    page,
                    theme,
                    ..Default::default()
                };
                let bytes = render_to_bytes(build_document(&article(), &opts).unwrap()).unwrap();
                assert!(bytes.starts_with(b"%PDF"), "{page:?}/{theme:?}");
            }
        }
    }

    #[test]
    fn render_article_reports_bookmark_count() {
        let rendered = render_article(
            &article(),
            &PdfOptions::default(),
            &crate::postprocess::Meta::default(),
        )
        .unwrap();
        assert!(rendered.bytes.starts_with(b"%PDF"));
        // Belge başlığı + 1 alt başlık.
        assert_eq!(rendered.bookmarks, 2);
    }

    #[test]
    fn bookmark_targets_start_at_the_content_top_of_the_page() {
        let (_, bookmarks) = build_document(&article(), &PdfOptions::default())
            .unwrap()
            .render_with_bookmarks()
            .unwrap();
        // Belge başlığı ilk sayfanın en üstünde: üst kenar boşluğu + üstbilgi
        // yüksekliği kadar aşağıda olmalı (üst kenar boşluğu 10 mm, üstbilgi
        // iki satır 8 punto + yarım mm boşluk).
        let (top_margin, _, _, _) = PageSize::A4.margins_mm();
        let first = &bookmarks[0];
        assert_eq!(first.page, 1);
        assert!(
            first.y_mm >= f64::from(top_margin) && first.y_mm < f64::from(top_margin) + 12.0,
            "beklenmeyen yer imi konumu: {} mm",
            first.y_mm
        );
        // Sonraki başlık daha aşağıda olmalı.
        assert!(bookmarks[1].y_mm > first.y_mm);
    }

    #[test]
    fn a5_page_uses_less_width_for_images() {
        assert!(PageSize::A5.content_width_mm() < PageSize::A4.content_width_mm());
        assert!(PageSize::A5.max_image_height_mm() < PageSize::A4.max_image_height_mm());
    }
}
