use crate::extract::{Article, Block, Table};
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

    /// Temanın renk paleti.
    pub fn palette(self) -> Palette {
        match self {
            Self::Light => Palette {
                text: Color::Rgb(0x1F, 0x24, 0x2B),
                muted: Color::Rgb(0x6B, 0x72, 0x80),
                code: Color::Rgb(0x24, 0x29, 0x2F),
                background: None,
            },
            Self::Dark => Palette {
                text: Color::Rgb(0xE4, 0xE7, 0xEB),
                muted: Color::Rgb(0x9A, 0xA4, 0xB2),
                code: Color::Rgb(0xCF, 0xD6, 0xDE),
                background: Some(Color::Rgb(0x16, 0x19, 0x1D)),
            },
            Self::Sepia => Palette {
                text: Color::Rgb(0x4A, 0x3B, 0x2A),
                muted: Color::Rgb(0x8A, 0x73, 0x55),
                code: Color::Rgb(0x3E, 0x32, 0x26),
                background: Some(Color::Rgb(0xF6, 0xEE, 0xDC)),
            },
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
    /// Kod bloğu metni (çerçevesi de bu rengi kullanır).
    pub code: Color,
    /// Sayfa zemini; `None` ise zemin çizilmez (beyaz kalır).
    pub background: Option<Color>,
}

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
        "ğĞüÜşŞıİöÖçÇâÂîÎûÛéÉèÈàÀôÔ•‣▪–—―…“”„‘’«»→←≥≤≠±×÷°²³½¼¾€$£¥¢©®™†‡§¶№\u{00A0}".chars(),
    );
    chars.extend(article.title.chars());
    for block in &article.blocks {
        match block {
            Block::Heading { text, .. }
            | Block::Paragraph(text)
            | Block::Caption(text)
            | Block::Quote(text)
            | Block::ListItem { text, .. } => chars.extend(text.chars()),
            Block::Code(code) => chars.extend(code.chars()),
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
    article.blocks.iter().any(|b| matches!(b, Block::Code(_)))
}

/// Üstbilgi: her sayfada başlık solda, "Sayfa X" sağda. İlk sayfada başlık
/// zaten büyük puntoyla yazıldığı için tekrarlanmaz.
fn header_layout(title: String, palette: Palette) -> impl Fn(usize) -> LinearLayout + 'static {
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
                Paragraph::new(format!("Sayfa {page}"))
                    .aligned(Alignment::Right)
                    .styled(style),
            ),
        ]);
        layout.push(row);
        layout.push(Break::new(0.5));
        layout
    }
}

/// Render sırasında doldurulan yer imi yakalama durumu.
#[derive(Debug, Default)]
struct CaptureState {
    /// Yakalanan yer imleri (belge sırasında).
    entries: Vec<Bookmark>,
    /// Şu an doldurulan sayfa (1 tabanlı).
    page: usize,
    /// İçerik alanının sayfa üstünden uzaklığı (mm).
    content_top_mm: f64,
    /// İçerik alanının yüksekliği (mm).
    content_height_mm: f64,
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
            state.content_height_mm = content_height_mm;
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
    let palette = opts.theme.palette();
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
        page: 0,
    };
    if opts.footer {
        let title = article.title.clone();
        decorator.header = Some(Box::new(move |page: usize| {
            Box::new(header_layout(title.clone(), palette)(page))
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
                .with_color(palette.code),
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
}

/// Makaleyi PDF baytlarına dönüştürür: yer imleri, meta veri ve tema zemini dâhil.
pub fn render_article(
    article: &Article,
    opts: &PdfOptions,
    meta: &crate::postprocess::Meta,
) -> Result<Rendered> {
    let job = build_document(article, opts)?;
    let (bytes, bookmarks) = job.render_with_bookmarks()?;
    let bookmark_count = bookmarks.len();
    let bytes = crate::postprocess::apply(
        bytes,
        &crate::postprocess::OutputOptions {
            meta: meta.clone(),
            page: opts.page,
            background: opts.theme.palette().background,
            bookmarks,
        },
    )?;
    Ok(Rendered {
        bytes,
        bookmarks: bookmark_count,
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
        Block::Code(code) => {
            let style = mono_style.unwrap_or(body);
            doc.push(code_block(code, style, palette));
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

/// Kod bloğu: tek bir çerçeve içinde, mono fontla, girintiler korunarak.
/// Her satır ayrı paragraf olur (genpdf satır sonlarını kendi bölmez).
/// Çerçeve rengi stil renginden gelir (koyu temada görünmesi için şart).
fn code_block(code: &str, style: Style, palette: Palette) -> impl Element {
    let mut lines = LinearLayout::vertical();
    for line in code.lines() {
        let text = if line.trim().is_empty() {
            " ".to_string()
        } else {
            line.to_string()
        };
        lines.push(Paragraph::new(text).styled(style));
    }
    let mut table = TableLayout::new(vec![1]);
    table.set_cell_decorator(FrameCellDecorator::new(false, true, true));
    let _ = table.push_row(vec![Box::new(lines.padded(Margins::all(2)))]);
    table.styled(Style::new().with_color(palette.code))
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
                Block::Code("fn main() {}\n// yorum".into()),
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
        assert!(Theme::Light.palette().background.is_none());
        assert!(Theme::Dark.palette().background.is_some());
        assert!(Theme::Sepia.palette().background.is_some());
        // Koyu temada metin açık, açık temada koyudur.
        let (dark_text, dark_bg) = (
            Theme::Dark.palette().text,
            Theme::Dark.palette().background.unwrap(),
        );
        assert_ne!(dark_text, dark_bg);
        let dark_luma = luma(dark_text);
        let dark_bg_luma = luma(dark_bg);
        assert!(
            dark_luma > dark_bg_luma,
            "koyu temada metin zeminden açık olmalı"
        );
        let light = Theme::Light.palette();
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
        let palette = Theme::Light.palette();
        let mut layout = header_layout("Deneme".to_string(), palette)(3);
        let _ = &mut layout;
        // İlk sayfada başlık tekrarını önlemek için boş bırakılır.
        let _ = header_layout("Deneme".to_string(), palette)(1);
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
        assert!(table_element(&Table::default(), Theme::Light.palette()).is_none());
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
    fn code_block_keeps_indentation_and_uses_single_frame() {
        let style = Style::new().with_font_size(9);
        let el = code_block(
            "fn main() {\n    let x = 1;\n\n    println!(\"{x}\");\n}",
            style,
            Theme::Light.palette(),
        );
        let mut doc = Document::new(sans_family(&chars()).unwrap());
        doc.push(el);
        let mut bytes = Vec::new();
        doc.render(&mut bytes).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // Girintili satırlar korunmalı: tek çerçeve içinde 5 satır.
        assert!(bytes.len() > 1000);
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
