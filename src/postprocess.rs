//! Render sonrası PDF işlemleri (`lopdf`): meta veri, seviyeli yer imi ağacı ve
//! tema zemini.
//!
//! Bu üç iş genpdf/printpdf katmanında mümkün değil:
//! - printpdf yalnızca *düz* yer imi (sayfa -> ad) yazar; biz seviyeli bir
//!   içindekiler ağacı istiyoruz,
//! - `/Info` sözlüğüne başlık/yazar/tarih/dil alanlarını genpdf yazmaz,
//! - koyu/sepya temada sayfanın tamamını kaplayan zemin dolgusu, eleman
//!   API'sinde yok (kenar boşlukları da kaplanmalı).
//!
//! Üçü de render edilmiş PDF üzerinde tek bir turda uygulanır. Yer imlerinin
//! sayfa/konum bilgisi render sırasında `pdf::Bookmark` olarak yakalanır.

use crate::pdf::{Bookmark, CodeBoxRect, CodeDecoration, CodePalette, PageSize};
use anyhow::{Context, Result};
use genpdf::style::Color;
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};
use std::collections::BTreeMap;

/// 1 mm kaç punto eder.
const MM_TO_PT: f64 = 72.0 / 25.4;

/// PDF meta verisi.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// Belge başlığı (`/Title`).
    pub title: String,
    /// Yazar/kaynak site (`/Author`).
    pub author: String,
    /// Ana dil etiketi, ör. `tr` (`/Lang` ve `/Info`).
    pub language: String,
}

/// Sonlandırma seçenekleri.
#[derive(Debug, Clone)]
pub struct OutputOptions {
    /// Meta veri.
    pub meta: Meta,
    /// Sayfa boyutu (yer imi hedefleri için).
    pub page: PageSize,
    /// Sayfa zemini; `None` ise zemin çizilmez.
    pub background: Option<Color>,
    /// Kod kutusu paleti (zemin, çerçeve, sol şerit); `None` ise boyanmaz.
    pub code: Option<CodePalette>,
    /// Kod kutularının sayfa/konumları (mm, sayfa üstünden).
    pub code_boxes: Vec<CodeBoxRect>,
    /// Blogun vurguladığı kod satırlarının bantları (mm, sayfa üstünden).
    /// Kutu zemininden **sonra**, kod metninden **önce** basılır.
    pub code_highlights: Vec<CodeBoxRect>,
    /// Kod kutusunda kopyalanmaması gereken metin öbekleri (satır numarası
    /// sütunu, dil rozeti).
    pub code_decorations: Vec<CodeDecoration>,
    /// Yer imleri (belge sırasında).
    pub bookmarks: Vec<Bookmark>,
}

/// Üretilmiş PDF'e meta veri, yer imi ağacı ve tema zeminini uygular.
pub fn apply(bytes: Vec<u8>, options: &OutputOptions) -> Result<Vec<u8>> {
    if options.bookmarks.is_empty()
        && options.background.is_none()
        && options.code_boxes.is_empty()
        && options.code_highlights.is_empty()
        && options.meta == Meta::default()
    {
        return Ok(bytes);
    }

    let mut doc = Document::load_mem(&bytes).context("üretilen PDF yeniden okunamadı")?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Ok(bytes);
    }

    if options.background.is_some()
        || !options.code_boxes.is_empty()
        || !options.code_highlights.is_empty()
    {
        paint_backgrounds(&mut doc, &pages, options)?;
    }
    if !options.code_decorations.is_empty() {
        hide_code_decorations(&mut doc, &pages, options)?;
    }
    set_metadata(&mut doc, &options.meta)?;
    if !options.bookmarks.is_empty() {
        if let Some(root) = write_outlines(&mut doc, &options.bookmarks, &pages, options.page) {
            attach_outlines(&mut doc, root)?;
        }
    }
    compress_streams(&mut doc)?;

    let mut out = Vec::with_capacity(bytes.len() + 2048);
    doc.save_to(&mut out).context("PDF sonlandırılamadı")?;
    Ok(out)
}

// ------------------------------------------------------- kopyalanabilir kod

/// Kod süslerini (satır numarası sütunu, `│`/`»` işaretleri, dil rozeti)
/// içerik akışında **boş bir `ActualText` ile işaretler**.
///
/// Numaralar PDF'te gerçek metin olarak basılır (görünmeleri ve satırların
/// kaymaması için gerekli); seç-kopyala sırasında kodun içine karışmamaları
/// ise ancak `ActualText` ile sağlanır: PDF okuyucuları işaretli alanın metnini
/// yok sayar, yani kopyalanan şey yalnızca kaynak kod olur.
///
/// Eşleşme **katıdır**: metin gösterimi kaydedilen alanın içinde başlamalı ve
/// glif sayısı birebir tutmalıdır. Sayılar uyuşmazsa hiçbir şey gizlenmez —
/// gevşek bir eşleşme kodun kendisini kopyalanamaz hâle getirirdi.
fn hide_code_decorations(
    doc: &mut Document,
    pages: &BTreeMap<u32, ObjectId>,
    options: &OutputOptions,
) -> Result<()> {
    for (page_number, page_id) in pages {
        let rects: Vec<HideRect> = options
            .code_decorations
            .iter()
            .filter(|decoration| decoration.page == *page_number)
            .map(|decoration| decoration_rect(decoration, options.page))
            .collect();
        if rects.is_empty() {
            continue;
        }
        for content_id in doc.get_page_contents(*page_id) {
            let stream = doc
                .get_object_mut(content_id)
                .context("sayfa içeriği bulunamadı")?
                .as_stream_mut()
                .context("sayfa içeriği akış değil")?;
            let content = stream_content(stream)?;
            let (rewritten, hidden) = mark_hidden_text(&content, &rects);
            if hidden == 0 {
                continue;
            }
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(rewritten);
            stream.compress().context("sayfa içeriği sıkıştırılamadı")?;
        }
    }
    Ok(())
}

/// Boşaltılacak metin göbeği (PDF noktası, sol-alt köşe başlangıç).
#[derive(Debug, Clone, Copy)]
struct HideRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// Öbeğin basması beklenen glif sayısı.
    glyphs: usize,
}

/// mm cinsinden süs konumunu PDF noktasına çevirir (PDF koordinatı sol-altta).
fn decoration_rect(decoration: &CodeDecoration, page: PageSize) -> HideRect {
    let (_, page_height_mm) = page.dimensions_mm();
    HideRect {
        x: decoration.x_mm * MM_TO_PT,
        y: (page_height_mm - decoration.y_mm - decoration.height_mm) * MM_TO_PT,
        width: decoration.width_mm * MM_TO_PT,
        height: decoration.height_mm * MM_TO_PT,
        glyphs: decoration.glyphs,
    }
}

/// Sayfa içerik akışında, `rects` alanlarına düşen **ilk** metin gösterimlerini
/// `ActualText` ile boşaltır. (Yeni akış, gizlenen öbek sayısı) döner.
fn mark_hidden_text(content: &[u8], rects: &[HideRect]) -> (Vec<u8>, usize) {
    /// Konum eşleşmesinde hoş görülen sapma (pt).
    const TOLERANCE_PT: f64 = 0.8;
    /// Boş `ActualText`: işaretli alan çıkarımda hiç metin üretmez.
    const MARK_START: &[u8] = b"BDC\n/Span<</ActualText()>>\n";
    const MARK_END: &[u8] = b"\nEMC\n";

    // (ofset, eklenecek baytlar, bu ofsetin önüne mi eklenir?)
    let mut inserts: Vec<(usize, &'static [u8], bool)> = Vec::new();
    let mut hidden = 0usize;

    let mut numbers: Vec<f64> = Vec::new();
    let mut position = (0.0f64, 0.0f64);
    let mut in_text = false;
    let mut first_show = false;
    // Bir metin gösteriminin operanı: (başlangıç, bitiş, glif sayısı).
    let mut pending: Option<(usize, usize, Option<usize>)> = None;

    let mut i = 0usize;
    while i < content.len() {
        match content[i] {
            b'%' => {
                while i < content.len() && content[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' => {
                // Gerçek (literal) dize: kaçış ve iç içe parantezler.
                let start = i;
                i += 1;
                let mut depth = 1usize;
                while i < content.len() && depth > 0 {
                    match content[i] {
                        b'\\' => i += 2,
                        b'(' => {
                            depth += 1;
                            i += 1;
                        }
                        b')' => {
                            depth -= 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                let end = i.min(content.len());
                pending = Some((start, end, None));
            }
            b'<' if content.get(i + 1) == Some(&b'<') => {
                i += 2;
                numbers.clear();
            }
            b'>' => i += 1,
            b'<' => {
                // Onaltılık dize: her glif iki basamaktır.
                let start = i;
                i += 1;
                let mut digits = 0usize;
                while i < content.len() && content[i] != b'>' {
                    if content[i].is_ascii_hexdigit() {
                        digits += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(content.len());
                pending = Some((start, i, Some(digits / 2)));
            }
            b'[' => {
                let start = i;
                i += 1;
                let mut digits = 0usize;
                let mut depth = 1usize;
                while i < content.len() && depth > 0 {
                    match content[i] {
                        b'[' => {
                            depth += 1;
                            i += 1;
                        }
                        b']' => {
                            depth -= 1;
                            i += 1;
                        }
                        b'<' if content.get(i + 1) != Some(&b'<') => {
                            i += 1;
                            while i < content.len() && content[i] != b'>' {
                                if content[i].is_ascii_hexdigit() {
                                    digits += 1;
                                }
                                i += 1;
                            }
                            i = (i + 1).min(content.len());
                        }
                        b'(' => {
                            // Dizi içinde literal dize: glif sayısı bilinmez.
                            digits = usize::MAX;
                            i += 1;
                            let mut inner = 1usize;
                            while i < content.len() && inner > 0 {
                                match content[i] {
                                    b'\\' => i += 2,
                                    b'(' => {
                                        inner += 1;
                                        i += 1;
                                    }
                                    b')' => {
                                        inner -= 1;
                                        i += 1;
                                    }
                                    _ => i += 1,
                                }
                            }
                        }
                        _ => i += 1,
                    }
                }
                let glyphs = (digits != usize::MAX).then_some(digits / 2);
                pending = Some((start, i, glyphs));
            }
            b'/' => {
                i += 1;
                while i < content.len() && !is_content_delimiter(content[i]) {
                    i += 1;
                }
            }
            c if c.is_ascii_digit() || c == b'+' || c == b'-' || c == b'.' => {
                let start = i;
                i += 1;
                while i < content.len() && !is_content_delimiter(content[i]) {
                    i += 1;
                }
                if let Ok(number) = std::str::from_utf8(&content[start..i]).unwrap_or("").parse() {
                    numbers.push(number);
                }
            }
            c if c.is_ascii_whitespace() => i += 1,
            b']' => i += 1,
            _ => {
                // Operatör.
                let start = i;
                while i < content.len() && !is_content_delimiter(content[i]) {
                    i += 1;
                }
                let operator = &content[start..i];
                if in_text && first_show && matches!(operator, b"TJ" | b"Tj" | b"'" | b"\"") {
                    if let Some((operand_start, _, glyphs)) = pending {
                        if let Some(glyphs) = glyphs.filter(|glyphs| *glyphs > 0) {
                            if let Some(rect) = rects.iter().find(|rect| {
                                rect.glyphs == glyphs
                                    && position.0 >= rect.x - TOLERANCE_PT
                                    && position.0 <= rect.x + rect.width + TOLERANCE_PT
                                    && position.1 >= rect.y - TOLERANCE_PT
                                    && position.1 <= rect.y + rect.height + TOLERANCE_PT
                            }) {
                                let _ = rect;
                                inserts.push((operand_start, MARK_START, true));
                                inserts.push((i, MARK_END, false));
                                hidden += 1;
                            }
                        }
                        first_show = false;
                    }
                }
                match operator {
                    b"BT" => {
                        in_text = true;
                        first_show = true;
                        position = (0.0, 0.0);
                    }
                    b"ET" => {
                        in_text = false;
                        first_show = false;
                    }
                    b"Td" | b"TD" if numbers.len() >= 2 => {
                        if !in_text {
                            position = (0.0, 0.0);
                        }
                        position.0 += numbers[numbers.len() - 2];
                        position.1 += numbers[numbers.len() - 1];
                    }
                    b"Tm" if numbers.len() >= 6 => {
                        position = (numbers[numbers.len() - 2], numbers[numbers.len() - 1]);
                    }
                    _ => {}
                }
                numbers.clear();
                pending = None;
            }
        }
    }

    if inserts.is_empty() {
        return (content.to_vec(), 0);
    }
    inserts.sort_by_key(|(offset, _, _)| *offset);
    let mut out = Vec::with_capacity(content.len() + inserts.len() * 32);
    let mut cursor = 0usize;
    for (offset, bytes, _before) in inserts {
        let offset = offset.min(content.len());
        // `before/after` ayrımı akış düzeninde aynı sonucu verir: her ekleme
        // kendi ofsetinden önce yazılır, içerik sırası korunur.
        out.extend_from_slice(&content[cursor..offset]);
        out.extend_from_slice(bytes);
        cursor = offset;
    }
    out.extend_from_slice(&content[cursor..]);
    (out, hidden)
}

/// İçerik akışı belirteç ayracı (PDF sözdizimi).
fn is_content_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'/' | b'[' | b']' | b'<' | b'>' | b'(' | b')' | b'%')
}

// ----------------------------------------------------------------- sıkıştırma

/// Sıkıştırılmamış akışları kayıpsız Flate ile paketler (görseller dâhil).
///
/// printpdf görselleri **hiç sıkıştırmadan** gömer: 2784×1824 piksellik bir
/// ekran görüntüsü PDF'te 15 MB ham RGB olarak durur (MDN JavaScript rehberi
/// böyle 15,6 MB oluyordu). Ekran görüntüleri `FlateDecode` ile 20-27 kat
/// küçülür ve hiçbir piksel değişmez; fotoğraflarda kazanç daha azdır ama
/// kayıp da yoktur. `Stream::compress()` yalnızca gerçekten küçüldüğünde
/// uygular, yani var olan sıkıştırılmış akışlara dokunulmaz.
fn compress_streams(doc: &mut Document) -> Result<()> {
    for object in doc.objects.values_mut() {
        if let Object::Stream(stream) = object {
            stream.compress().context("akış sıkıştırılamadı")?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- zemin

/// Her sayfanın içerik akışının başına tam sayfa dolgu ekler.
///
/// Operatörler akışın **başına** eklenir, yani metnin arkasında kalırlar.
/// Sıra önemlidir: önce sayfa zemini, sonra kod kutuları. Tersi olsaydı zemin
/// kod kutusunu örterdi. Sayfa zemini kenar boşluklarını da kapladığı için
/// sayfa ölçüsündedir; kod kutuları metin sütununa oturur.
fn paint_backgrounds(
    doc: &mut Document,
    pages: &BTreeMap<u32, ObjectId>,
    options: &OutputOptions,
) -> Result<()> {
    let page_background = options
        .background
        .map(|color| page_background_operators(options.page, color));

    for (page_number, page_id) in pages {
        let mut prefix = String::new();
        if let Some(operators) = &page_background {
            prefix.push_str(operators);
        }
        if let Some(palette) = options.code {
            for rect in options
                .code_boxes
                .iter()
                .filter(|rect| rect.page == *page_number)
            {
                prefix.push_str(&code_box_operators(rect, options.page, palette));
            }
            // Vurgu bantları kutunun *üstüne* gelir (kutu zemini onları
            // örtmesin) ama metin akıştan sonra basıldığı için kod metninin
            // arkasında kalır.
            for band in options
                .code_highlights
                .iter()
                .filter(|band| band.page == *page_number)
            {
                prefix.push_str(&highlight_band_operators(band, options.page, palette));
            }
        }
        if prefix.is_empty() {
            continue;
        }
        let operators = prefix.into_bytes();

        let contents = doc.get_page_contents(*page_id);
        if contents.is_empty() {
            // İçeriksiz sayfa (olmaması beklenir): zemin için akış oluştur.
            let mut stream = lopdf::Stream::new(Dictionary::new(), operators.clone());
            stream.compress().context("zemin akışı sıkıştırılamadı")?;
            let id = doc.add_object(stream);
            let page = doc.get_object_mut(*page_id)?.as_dict_mut()?;
            page.set("Contents", Object::Reference(id));
            continue;
        }
        for content_id in contents {
            let stream = doc
                .get_object_mut(content_id)
                .context("sayfa içeriği bulunamadı")?
                .as_stream_mut()
                .context("sayfa içeriği akış değil")?;
            let mut content = stream_content(stream)?;
            content.splice(0..0, operators.iter().copied());
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(content);
            stream.compress().context("sayfa içeriği sıkıştırılamadı")?;
        }
    }
    Ok(())
}

/// Sayfa zeminini işaretleyen içerik akışı yorumu (testler de arar).
const PAGE_BACKGROUND_MARKER: &str = "% snappdf:zemin";

/// Kod kutusu dolgusunu işaretleyen içerik akışı yorumu.
const CODE_BOX_MARKER: &str = "% snappdf:kod-kutusu";

/// Tam sayfayı kaplayan zemin dolgusu.
fn page_background_operators(page: PageSize, color: Color) -> String {
    let (red, green, blue) = rgb01(color);
    let (width_mm, height_mm) = page.dimensions_mm();
    format!(
        "{PAGE_BACKGROUND_MARKER}\nq {:.4} {:.4} {:.4} rg 0 0 {:.2} {:.2} re f Q\n",
        red,
        green,
        blue,
        width_mm * MM_TO_PT,
        height_mm * MM_TO_PT
    )
}

/// Kod kutusu köşe yarıçapı (mm): bloglardaki yumuşak köşeler.
///
/// Şeritten (bkz. `CODE_ACCENT_WIDTH_MM`) küçük seçilir; böylece şeridin sol
/// yayları kutunun sol yaylarıyla birebir çakışır, şerit köşelerden dışarı
/// taşmaz.
pub const CODE_CORNER_RADIUS_MM: f64 = 1.0;

/// Kutunun sol kenarındaki renk şeridinin genişliği (mm).
pub const CODE_ACCENT_WIDTH_MM: f64 = 1.1;

/// Kutunun ince çerçevesinin kalınlığı (pt).
const CODE_BORDER_WIDTH_PT: f64 = 0.8;

/// Kod kutusu: yuvarlatılmış zemin + çerçeve ve sol renk şeridi.
///
/// PDF koordinatları sol-alt köşeden başlar; yakalanan dikdörtgen ise sayfanın
/// üst-sol köşesine göredir. Zemin ve çerçeve tek yolda `B` ile (dolgu + çizgi)
/// basılır, şerit onun üstüne gelir.
fn code_box_operators(rect: &CodeBoxRect, page: PageSize, palette: CodePalette) -> String {
    let (_, page_height_mm) = page.dimensions_mm();
    let x = rect.x_mm * MM_TO_PT;
    let y = (page_height_mm - rect.y_mm - rect.height_mm) * MM_TO_PT;
    let width = rect.width_mm * MM_TO_PT;
    let height = rect.height_mm * MM_TO_PT;
    let radius = CODE_CORNER_RADIUS_MM * MM_TO_PT;
    let strip = CODE_ACCENT_WIDTH_MM * MM_TO_PT;
    let (fr, fg, fb) = rgb01(palette.background);
    let (br, bg, bb) = rgb01(palette.border);
    let (ar, ag, ab) = rgb01(palette.accent);
    format!(
        // Geometri, yorum satırında da yazılır: testler ve hata ayıklama
        // doğrudan buradan okur (`%` PDF içerik akışında yorumdur).
        "{CODE_BOX_MARKER} {x:.2} {y:.2} {width:.2} {height:.2}\n\
         q\n\
         {fr:.4} {fg:.4} {fb:.4} rg {br:.4} {bg:.4} {bb:.4} RG {CODE_BORDER_WIDTH_PT:.2} w\n\
         {box_path}B\n\
         {ar:.4} {ag:.4} {ab:.4} rg\n\
         {strip_path}f\n\
         Q\n",
        box_path = rounded_rect_path(x, y, width, height, [radius; 4]),
        // Şerit yalnızca sol köşelerde yuvarlatılır; sağ kenarı düz kalır.
        strip_path = rounded_rect_path(x, y, strip, height, [radius, 0.0, 0.0, radius]),
    )
}

/// Vurgulu kod satırının zemin bandı: düz dolgu, çerçevesiz.
///
/// Bant kutunun iç dolgusundan sonra başlar; böylece ne sol şeride ne de
/// yuvarlatılmış köşelere değer (yarıçap iç boşluktan küçüktür).
fn highlight_band_operators(band: &CodeBoxRect, page: PageSize, palette: CodePalette) -> String {
    let (_, page_height_mm) = page.dimensions_mm();
    let x = band.x_mm * MM_TO_PT;
    let y = (page_height_mm - band.y_mm - band.height_mm) * MM_TO_PT;
    let width = band.width_mm * MM_TO_PT;
    let height = band.height_mm * MM_TO_PT;
    let (r, g, b) = rgb01(palette.highlight);
    format!(
        "{HIGHLIGHT_MARKER} {x:.2} {y:.2} {width:.2} {height:.2}\n\
         q\n\
         {r:.4} {g:.4} {b:.4} rg\n\
         {x:.2} {y:.2} {width:.2} {height:.2} re\n\
         f\n\
         Q\n"
    )
}

/// Vurgu bandını işaretleyen içerik akışı yorumu (testler de arar).
const HIGHLIGHT_MARKER: &str = "% snappdf:satir-vurgusu";

/// Yuvarlatılmış dikdörtgen yolu (PDF koordinatları, sol-alt köşe başlangıç).
///
/// `radii` sırası: sol-alt, sağ-alt, sağ-üst, sol-üst. 0 yarıçap keskin köşe
/// demektir. Yay, çeyrek daire için standart 0.5523 katsayılı Bézier
/// eğrisiyle yaklaşıklanır.
fn rounded_rect_path(x: f64, y: f64, width: f64, height: f64, radii: [f64; 4]) -> String {
    const K: f64 = 0.5523;
    let [bl, br, tr, tl] = fit_radii(width, height, radii);
    let (x1, y1) = (x + width, y + height);
    let mut path = String::with_capacity(256);
    // Sol-alt yayının bittiği noktadan başla.
    path.push_str(&format!("{:.2} {:.2} m\n", x + bl, y));
    // Sağ-alt köşe.
    if br > 0.0 {
        path.push_str(&format!("{:.2} {:.2} l\n", x1 - br, y));
        path.push_str(&format!(
            "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
            x1 - br + K * br,
            y,
            x1,
            y + br - K * br,
            x1,
            y + br
        ));
    } else {
        path.push_str(&format!("{:.2} {:.2} l\n", x1, y));
    }
    // Sağ-üst köşe.
    if tr > 0.0 {
        path.push_str(&format!("{:.2} {:.2} l\n", x1, y1 - tr));
        path.push_str(&format!(
            "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
            x1,
            y1 - tr + K * tr,
            x1 - tr + K * tr,
            y1,
            x1 - tr,
            y1
        ));
    } else {
        path.push_str(&format!("{:.2} {:.2} l\n", x1, y1));
    }
    // Sol-üst köşe.
    if tl > 0.0 {
        path.push_str(&format!("{:.2} {:.2} l\n", x + tl, y1));
        path.push_str(&format!(
            "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
            x + tl - K * tl,
            y1,
            x,
            y1 - tl + K * tl,
            x,
            y1 - tl
        ));
    } else {
        path.push_str(&format!("{:.2} {:.2} l\n", x, y1));
    }
    // Sol-alt köşe: başlangıç noktasına kapanış.
    if bl > 0.0 {
        path.push_str(&format!("{:.2} {:.2} l\n", x, y + bl));
        path.push_str(&format!(
            "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
            x,
            y + bl - K * bl,
            x + bl - K * bl,
            y,
            x + bl,
            y
        ));
    } else {
        path.push_str(&format!("{:.2} {:.2} l\n", x, y));
    }
    path.push_str("h\n");
    path
}

/// Yarıçapları dikdörtgene sığdırır: aynı kenarı paylaşan iki yay o kenarı
/// aşamaz (aksi hâlde yol kendini keser ve köşe bozulur).
fn fit_radii(width: f64, height: f64, radii: [f64; 4]) -> [f64; 4] {
    let [bl, br, tr, tl] = radii.map(|radius| radius.max(0.0));
    let pair = |first: f64, second: f64| (first + second).max(1e-6);
    let scale = [
        width / pair(bl, br),  // alt kenar
        width / pair(tl, tr),  // üst kenar
        height / pair(bl, tl), // sol kenar
        height / pair(br, tr), // sağ kenar
    ]
    .into_iter()
    .fold(1.0f64, f64::min);
    [bl, br, tr, tl].map(|radius| radius * scale)
}

/// Bir içerik akışının açılmış baytları.
///
/// Sıkıştırılmamış akışlarda `/Filter` bulunmaz ve lopdf'in
/// `decompressed_content`ı bu durumda hata verir; o hâlde ham içerik kullanılır.
fn stream_content(stream: &lopdf::Stream) -> Result<Vec<u8>> {
    if stream.dict.get(b"Filter").is_err() {
        return Ok(stream.content.clone());
    }
    stream
        .decompressed_content()
        .context("sayfa içeriği açılamadı")
}

/// Renk bileşenlerini PDF operatörleri için 0-1 aralığına indirger.
fn rgb01(color: Color) -> (f64, f64, f64) {
    let (red, green, blue) = rgb(color);
    (
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
    )
}

/// Renk bileşenlerini 0-255 aralığına indirger.
fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(red, green, blue) => (red, green, blue),
        Color::Greyscale(value) => (value, value, value),
        // CMYK zemin desteği kullanılmıyor; beyaza düşer.
        Color::Cmyk(..) => (255, 255, 255),
    }
}

// -------------------------------------------------------------- meta veri

/// `/Info` sözlüğünü ve `/Lang` etiketini yazar.
fn set_metadata(doc: &mut Document, meta: &Meta) -> Result<()> {
    let info_id = match doc.trailer.get(b"Info").and_then(Object::as_reference) {
        Ok(id) => id,
        Err(_) => {
            let id = doc.add_object(Dictionary::new());
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
    };
    let date = pdf_date_now();
    let dictionary = doc.get_object_mut(info_id)?.as_dict_mut()?;
    if !meta.title.is_empty() {
        dictionary.set("Title", text_object(&meta.title));
    }
    if !meta.author.is_empty() {
        dictionary.set("Author", text_object(&meta.author));
    }
    dictionary.set("Creator", text_object("snappdf"));
    dictionary.set("Producer", text_object("snappdf"));
    dictionary.set(
        "CreationDate",
        Object::String(date.clone().into_bytes(), StringFormat::Literal),
    );
    dictionary.set(
        "ModDate",
        Object::String(date.into_bytes(), StringFormat::Literal),
    );

    if !meta.language.is_empty() {
        if let Ok(catalog_id) = doc.trailer.get(b"Root").and_then(Object::as_reference) {
            if let Ok(catalog) = doc.get_object_mut(catalog_id).and_then(Object::as_dict_mut) {
                catalog.set(
                    "Lang",
                    Object::String(meta.language.clone().into_bytes(), StringFormat::Literal),
                );
            }
        }
    }
    Ok(())
}

/// Metni PDF metin dizesine çevirir. UTF-16BE (BOM ile) + onaltılık gösterim:
/// Türkçe karakterler ve kaçış gerektiren baytlar sorunsuz yazılır.
fn text_object(text: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

/// Şu anki zamanı PDF tarih dizesi olarak döndürür (`D:YYYYMMDDHHmmSSZ`, UTC).
fn pdf_date_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    pdf_date_from_unix(seconds)
}

/// Unix zamanını PDF tarih dizesine çevirir.
fn pdf_date_from_unix(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z")
}

/// Gün sayısını (1970-01-01 = 0) takvim tarihine çevirir (Howard Hinnant
/// algoritması; artık yılları ve 1970 öncesini de doğru işler).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

// -------------------------------------------------------------- yer imleri

/// Yer imi ağacı (indeks tabanlı).
#[derive(Debug, Default, PartialEq, Eq)]
struct OutlineTree {
    /// Her düğümün ebeveyni.
    parent: Vec<Option<usize>>,
    /// Her düğümün çocukları (belge sırasında).
    children: Vec<Vec<usize>>,
    /// Kök seviyesindeki düğümler.
    roots: Vec<usize>,
}

/// Yer imlerini seviyelerine göre ağaca dizer: her yer imi, kendisinden önce
/// gelen ve daha küçük seviyeli en yakın yer iminin çocuğu olur. Böylece
/// `h2` altında `h3`, `h1` altında `h2` toplanır ve seviye atlamaları da
/// (ör. `h2`dan `h4`e) doğru ebeveyne bağlanır.
fn outline_tree(bookmarks: &[Bookmark]) -> OutlineTree {
    let mut tree = OutlineTree {
        parent: vec![None; bookmarks.len()],
        children: vec![Vec::new(); bookmarks.len()],
        roots: Vec::new(),
    };
    let mut stack: Vec<usize> = Vec::new();
    for index in 0..bookmarks.len() {
        while let Some(&top) = stack.last() {
            if bookmarks[top].level < bookmarks[index].level {
                break;
            }
            stack.pop();
        }
        match stack.last() {
            Some(&parent) => {
                tree.parent[index] = Some(parent);
                tree.children[parent].push(index);
            }
            None => tree.roots.push(index),
        }
        stack.push(index);
    }
    tree
}

/// Her düğümün altındaki (tüm seviyelerdeki) öğe sayısı — `/Count` için.
/// Ağaç açık gösterildiğinden görünür öğe sayısına eşittir.
fn descendant_counts(children: &[Vec<usize>]) -> Vec<usize> {
    fn walk(children: &[Vec<usize>], index: usize, counts: &mut [Option<usize>]) -> usize {
        if let Some(value) = counts[index] {
            return value;
        }
        let total = children[index].len()
            + children[index]
                .iter()
                .map(|&child| walk(children, child, counts))
                .sum::<usize>();
        counts[index] = Some(total);
        total
    }
    let mut counts = vec![None; children.len()];
    for index in 0..children.len() {
        walk(children, index, &mut counts);
    }
    counts.into_iter().map(|count| count.unwrap_or(0)).collect()
}

/// Yer imi ağacını PDF nesneleri olarak yazar ve kök nesnenin kimliğini
/// döndürür. `printpdf`in bıraktığı boş `/Outlines` nesnesi varsa o kullanılır.
fn write_outlines(
    doc: &mut Document,
    bookmarks: &[Bookmark],
    pages: &BTreeMap<u32, ObjectId>,
    page: PageSize,
) -> Option<ObjectId> {
    let tree = outline_tree(bookmarks);
    if tree.roots.is_empty() {
        return None;
    }
    let counts = descendant_counts(&tree.children);
    let ids: Vec<ObjectId> = (0..bookmarks.len()).map(|_| doc.new_object_id()).collect();
    let root_id = existing_outlines_id(doc).unwrap_or_else(|| doc.new_object_id());

    for index in 0..bookmarks.len() {
        let mut dictionary = Dictionary::new();
        dictionary.set("Title", text_object(&bookmarks[index].title));
        let parent = tree.parent[index].map_or(root_id, |parent| ids[parent]);
        dictionary.set("Parent", Object::Reference(parent));
        dictionary.set("Dest", destination(&bookmarks[index], pages, page));

        if let Some(previous) = previous_sibling(&tree, index) {
            dictionary.set("Prev", Object::Reference(ids[previous]));
        }
        if let Some(next) = next_sibling(&tree, index) {
            dictionary.set("Next", Object::Reference(ids[next]));
        }
        let children = &tree.children[index];
        if !children.is_empty() {
            dictionary.set("First", Object::Reference(ids[children[0]]));
            dictionary.set("Last", Object::Reference(ids[children[children.len() - 1]]));
            dictionary.set("Count", counts[index] as i64);
        }
        doc.objects
            .insert(ids[index], Object::Dictionary(dictionary));
    }

    let mut root = Dictionary::new();
    root.set("Type", Object::Name(b"Outlines".to_vec()));
    root.set("First", Object::Reference(ids[tree.roots[0]]));
    root.set(
        "Last",
        Object::Reference(ids[tree.roots[tree.roots.len() - 1]]),
    );
    // Kök, görünür tüm öğeleri sayar: kök seviyesindekiler + alt öğeleri.
    let total = tree.roots.len() + tree.roots.iter().map(|&root| counts[root]).sum::<usize>();
    root.set("Count", total as i64);
    doc.objects.insert(root_id, Object::Dictionary(root));
    Some(root_id)
}

/// Aynı seviyedeki bir önceki/ sonraki kardeş.
fn previous_sibling(tree: &OutlineTree, index: usize) -> Option<usize> {
    sibling(tree, index, -1)
}

/// Aynı seviyedeki bir sonraki kardeş.
fn next_sibling(tree: &OutlineTree, index: usize) -> Option<usize> {
    sibling(tree, index, 1)
}

fn sibling(tree: &OutlineTree, index: usize, step: i32) -> Option<usize> {
    let group = match tree.parent[index] {
        Some(parent) => &tree.children[parent],
        None => &tree.roots,
    };
    let position = group.iter().position(|&item| item == index)?;
    let target = position as i32 + step;
    if target < 0 {
        return None;
    }
    group.get(target as usize).copied()
}

/// Yer iminin hedefi: `[sayfa /XYZ sol üst null]`. Mevcut yakınlaştırmayı
/// korur, sayfayı yalnızca dikeyde konumlandırır.
fn destination(bookmark: &Bookmark, pages: &BTreeMap<u32, ObjectId>, page: PageSize) -> Object {
    let page_id = match pages.get(&bookmark.page).or_else(|| pages.values().next()) {
        Some(id) => *id,
        None => return Object::Null,
    };
    let (_, height_mm) = page.dimensions_mm();
    let top = (height_mm - bookmark.y_mm).clamp(0.0, height_mm) * MM_TO_PT;
    Object::Array(vec![
        Object::Reference(page_id),
        Object::Name(b"XYZ".to_vec()),
        Object::Integer(0),
        Object::Real(top),
        Object::Null,
    ])
}

/// Catalog'daki mevcut `/Outlines` nesnesi (printpdf boş bir tane bırakır).
fn existing_outlines_id(doc: &Document) -> Option<ObjectId> {
    doc.catalog()
        .ok()?
        .get(b"Outlines")
        .ok()?
        .as_reference()
        .ok()
}

/// Yer imi ağacını catalog'a bağlar ve PDF'i içindekiler görünümüyle açar.
fn attach_outlines(doc: &mut Document, root: ObjectId) -> Result<()> {
    let catalog_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .context("PDF catalog bulunamadı")?;
    let catalog = doc.get_object_mut(catalog_id)?.as_dict_mut()?;
    catalog.set("Outlines", Object::Reference(root));
    catalog.set("PageMode", Object::Name(b"UseOutlines".to_vec()));
    Ok(())
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Article, Block, Table};
    use crate::pdf::{CodeTheme, PdfOptions, Theme};

    fn bookmark(title: &str, level: u8, page: u32, y_mm: f64) -> Bookmark {
        Bookmark {
            title: title.to_string(),
            level,
            page,
            y_mm,
        }
    }

    /// PDF metin dizesini çözer (UTF-16BE BOM'lu veya düz baytlar).
    fn decoded(object: &Object) -> String {
        let bytes = object.as_str().expect("metin dizesi bekleniyordu");
        if bytes.starts_with(&[0xFE, 0xFF]) {
            let units: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(bytes).to_string()
        }
    }

    fn article() -> Article {
        Article {
            title: "Yer imli belge".into(),
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Birinci bölüm".into(),
                },
                Block::Paragraph("metin".into()),
                Block::Heading {
                    level: 3,
                    text: "Alt başlık".into(),
                },
                Block::Heading {
                    level: 2,
                    text: "İkinci bölüm".into(),
                },
            ],
            images: vec![],
        }
    }

    fn rendered(options: &PdfOptions) -> (Vec<u8>, Vec<Bookmark>) {
        let job = crate::pdf::build_document(&article(), options).unwrap();
        job.render_with_bookmarks().unwrap()
    }

    #[test]
    fn civil_dates_are_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        // 1972 artık yılı: 1972-02-29 = 790 gün.
        assert_eq!(civil_from_days(789), (1972, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(pdf_date_from_unix(0), "D:19700101000000Z");
        assert_eq!(pdf_date_from_unix(86_400), "D:19700102000000Z");
        assert_eq!(pdf_date_from_unix(1_700_000_000), "D:20231114221320Z");
    }

    #[test]
    fn outline_tree_nests_headings_by_level() {
        let bookmarks = vec![
            bookmark("Başlık", 0, 1, 0.0),
            bookmark("h2 a", 2, 1, 10.0),
            bookmark("h3 a1", 3, 1, 20.0),
            bookmark("h2 b", 2, 2, 30.0),
        ];
        let tree = outline_tree(&bookmarks);
        assert_eq!(tree.roots, vec![0]);
        assert_eq!(tree.children[0], vec![1, 3]);
        assert_eq!(tree.children[1], vec![2]);
        assert_eq!(tree.parent, vec![None, Some(0), Some(1), Some(0)]);
        assert_eq!(descendant_counts(&tree.children), vec![3, 1, 0, 0]);
    }

    #[test]
    fn outline_tree_handles_skipped_levels_and_multiple_roots() {
        let bookmarks = vec![
            bookmark("h1", 1, 1, 0.0),
            bookmark("h3 (atlama)", 3, 1, 5.0),
            bookmark("h1 b", 1, 2, 0.0),
        ];
        let tree = outline_tree(&bookmarks);
        assert_eq!(tree.roots, vec![0, 2]);
        assert_eq!(tree.children[0], vec![1]);
        assert_eq!(tree.parent[1], Some(0));
    }

    #[test]
    fn apply_sets_metadata_and_language() {
        let bytes = crate::pdf::render_article(
            &article(),
            &PdfOptions::default(),
            &Meta {
                title: "Türkçe Başlık".into(),
                author: "gencayyildiz.com".into(),
                language: "tr".into(),
            },
        )
        .unwrap()
        .bytes;
        let doc = Document::load_mem(&bytes).unwrap();
        let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info).unwrap();
        assert_eq!(decoded(info.get(b"Author").unwrap()), "gencayyildiz.com");
        assert!(info.get(b"Title").is_ok());
        assert!(info.get(b"CreationDate").is_ok());
        let catalog = doc.catalog().unwrap();
        assert_eq!(decoded(catalog.get(b"Lang").unwrap()), "tr");
        // Boş meta alanları /Info'ya yazılmaz.
        let empty =
            crate::pdf::render_article(&article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let doc = Document::load_mem(&empty).unwrap();
        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info_id).unwrap();
        assert!(info.get(b"Author").is_err());
        assert!(info.get(b"Lang").is_err());
    }

    #[test]
    fn apply_writes_hierarchical_outlines() {
        // Başlıklar + belge başlığı: ağaç kökü belge başlığı olmalı.
        let bytes =
            crate::pdf::render_article(&article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let doc = Document::load_mem(&bytes).unwrap();
        let catalog = doc.catalog().unwrap();
        assert_eq!(
            catalog.get(b"PageMode").unwrap().as_name().unwrap(),
            b"UseOutlines"
        );
        let root = catalog.get(b"Outlines").unwrap().as_reference().unwrap();
        let root = doc.get_dictionary(root).unwrap();
        assert_eq!(root.get(b"Type").unwrap().as_name().unwrap(), b"Outlines");
        // 4 yer imi (belge başlığı + 3 başlık) tek kök altında.
        assert_eq!(root.get(b"Count").unwrap().as_i64().unwrap(), 4);

        let first = root.get(b"First").unwrap().as_reference().unwrap();
        let first = doc.get_dictionary(first).unwrap();
        assert_eq!(decoded(first.get(b"Title").unwrap()), "Yer imli belge");
        assert!(first.get(b"Dest").is_ok());
        // Alt başlıklar ilk düğümün çocukları.
        assert!(first.get(b"First").is_ok());
    }

    #[test]
    fn apply_paints_requested_background() {
        let options = PdfOptions {
            theme: Theme::Dark,
            ..Default::default()
        };
        let bytes = crate::pdf::render_article(&article(), &options, &Meta::default())
            .unwrap()
            .bytes;
        let doc = Document::load_mem(&bytes).unwrap();
        let pages = doc.get_pages();
        let page_id = *pages.values().next().unwrap();
        let content_ids = doc.get_page_contents(page_id);
        assert!(!content_ids.is_empty());
        let stream = doc.get_object(content_ids[0]).unwrap().as_stream().unwrap();
        let text = String::from_utf8_lossy(&stream_content(stream).unwrap()).to_string();
        assert!(
            text.starts_with(PAGE_BACKGROUND_MARKER) && text.contains(" re f Q"),
            "zemin operatörleri bulunamadı: {}",
            &text[..text.len().min(80)]
        );
    }

    #[test]
    fn light_theme_does_not_paint_a_background() {
        let bytes =
            crate::pdf::render_article(&article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let doc = Document::load_mem(&bytes).unwrap();
        let pages = doc.get_pages();
        let content_ids = doc.get_page_contents(*pages.values().next().unwrap());
        let stream = doc.get_object(content_ids[0]).unwrap().as_stream().unwrap();
        let text = String::from_utf8_lossy(&stream_content(stream).unwrap()).to_string();
        assert!(!text.contains(PAGE_BACKGROUND_MARKER));
    }

    /// Tüm blok türlerini içeren belge (tema doğrulamaları için).
    fn full_article() -> Article {
        Article {
            title: "Tema denemesi".into(),
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Bölüm".into(),
                },
                Block::Paragraph("Gövde metni ğüşıöç.".into()),
                Block::Caption("Görsel açıklaması".into()),
                Block::Quote("Alıntı metni".into()),
                Block::Code {
                    text: "// yorum\nfn main() {}\n    println!(\"x\");".into(),
                    lang: Some("rust".into()),
                    highlights: vec![],
                },
                Block::Table(Table {
                    header: vec!["A".into(), "B".into()],
                    rows: vec![vec!["1".into(), "2".into()]],
                    columns: 2,
                }),
                Block::ListItem {
                    text: "madde bir".into(),
                    ordered: false,
                    depth: 0,
                },
                Block::ListItem {
                    text: "alt madde".into(),
                    ordered: true,
                    depth: 1,
                },
                Block::Divider,
                Block::Paragraph("Son paragraf.".into()),
            ],
            images: vec![],
        }
    }

    /// Belgenin tüm sayfalarındaki metin operatörlerini döndürür.
    fn content_streams(bytes: &[u8]) -> String {
        let doc = Document::load_mem(bytes).unwrap();
        let pages = doc.get_pages();
        let mut out = String::new();
        for page_id in pages.values() {
            for content_id in doc.get_page_contents(*page_id) {
                let stream = doc.get_object(content_id).unwrap().as_stream().unwrap();
                out.push_str(&String::from_utf8_lossy(&stream_content(stream).unwrap()));
            }
        }
        out
    }

    /// Her metin gösteriminden önce geçerli olan dolgu rengini döndürür.
    ///
    /// printpdf her metinden *sonra* dolgu rengini siyaha döndürür (`0 0 0 rg`),
    /// bu yüzden "akışta siyah var mı" bakmak yanıltıcıdır: renk verilmeyen bir
    /// eleman, önceki metnin bıraktığı siyahı kullanır. Ölçüt, `TJ` ile
    /// gerçekten glif basan her noktadan önceki rengin siyah olmamasıdır.
    fn text_fill_colors(stream: &str) -> Vec<(f64, f64, f64)> {
        let tokens: Vec<&str> = stream.split_whitespace().collect();
        let mut last = (0.0f64, 0.0f64, 0.0f64);
        let mut colors = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            if *token == "rg" && index >= 3 {
                let values: Vec<f64> = tokens[index - 3..index]
                    .iter()
                    .filter_map(|token| token.parse().ok())
                    .collect();
                if values.len() == 3 {
                    last = (values[0], values[1], values[2]);
                }
            } else if *token == "TJ" && index > 0 {
                // `[<..>] TJ` glif basar, `[] TJ` boş bir aralık gösterir.
                // Dizi genelde tek bir token olarak yazılır (`[<0024><0004>]`),
                // o yüzden `TJ`den önceki token'a bakmak yeterlidir; ayrık
                // yazım olasılığı için dizinin tamamı da taranır.
                let has_glyphs = tokens[index - 1].contains("<")
                    || tokens[..index]
                        .iter()
                        .rposition(|token| *token == "[")
                        .map(|open| tokens[open + 1..index].iter().any(|t| t.contains('<')))
                        .unwrap_or(false);
                if has_glyphs {
                    colors.push(last);
                }
            }
        }
        colors
    }

    #[test]
    fn dark_and_sepia_themes_never_print_default_black_text() {
        // Renk verilmeyen her metin siyah basılır ve koyu zeminde okunmaz olur.
        // Madde imleri (`•`) tam olarak böyle bir hata içeriyordu.
        for theme in [Theme::Dark, Theme::Sepia] {
            let options = PdfOptions {
                theme,
                ..Default::default()
            };
            let bytes = crate::pdf::render_article(&full_article(), &options, &Meta::default())
                .unwrap()
                .bytes;
            let colors = text_fill_colors(&content_streams(&bytes));
            assert!(!colors.is_empty(), "{theme:?}: hiç metin bulunamadı");
            let black = colors
                .iter()
                .filter(|(r, g, b)| *r == 0.0 && *g == 0.0 && *b == 0.0)
                .count();
            assert_eq!(
                black, 0,
                "{theme:?}: {black} metin varsayılan siyahla basılmış"
            );
        }
    }

    #[test]
    fn light_theme_prints_dark_text() {
        let bytes =
            crate::pdf::render_article(&full_article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let colors = text_fill_colors(&content_streams(&bytes));
        assert!(!colors.is_empty());
        // Açık temada hiçbir metin açık renkte olmamalı (zemin beyaz).
        assert!(
            colors.iter().all(|(r, g, b)| (*r + *g + *b) / 3.0 < 0.6),
            "açık temada açık renkli metin var"
        );
    }

    #[test]
    fn full_document_renders_with_tables_lists_code_and_quotes() {
        for page in [PageSize::A4, PageSize::A5] {
            let rendered = crate::pdf::render_article(
                &full_article(),
                &PdfOptions {
                    page,
                    ..Default::default()
                },
                &Meta {
                    title: "Tema denemesi".into(),
                    author: "test".into(),
                    language: "tr".into(),
                },
            )
            .unwrap();
            let bytes = rendered.bytes;
            // Belge başlığı + 1 başlık + madde imli liste ağacı.
            assert!(rendered.bookmarks >= 2, "{page:?}");
            let text = content_streams(&bytes);
            // Tablo ızgarası çizgi çizer, kod bloğu çerçeve çizer.
            assert!(!text.is_empty(), "{page:?}");
            let doc = Document::load_mem(&bytes).unwrap();
            let catalog = doc.catalog().unwrap();
            assert!(catalog.get(b"Outlines").is_ok(), "{page:?}");
            // Açık temada sayfa zemini çizilmez ama kod kutusu boyanır.
            assert!(
                !text.contains(PAGE_BACKGROUND_MARKER),
                "{page:?}: beklenmeyen sayfa zemini"
            );
            assert!(
                text.contains(CODE_BOX_MARKER),
                "{page:?}: kod kutusu zemini boyanmadı"
            );
        }
    }

    /// İçerik akışında `r g b rg` dolgu operatörü var mı? (printpdf renkleri
    /// iki ondalıkla yazar; mevcut testler de bu biçime dayanır.)
    fn prints_color(stream: &str, color: Color) -> bool {
        let (red, green, blue) = rgb(color);
        stream.contains(&format!(
            "{:.2} {:.2} {:.2} rg",
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0
        ))
    }

    /// `% snappdf:kod-kutusu` bloğunu çözer: işaret satırındaki geometri ve
    /// renk satırındaki dolgu -> (renk, [x, y, genişlik, yükseklik]).
    fn code_box_fill(stream: &str) -> Option<((f64, f64, f64), [f64; 4])> {
        let after = stream.split(CODE_BOX_MARKER).nth(1)?;
        let mut lines = after.lines();
        let geometry: Vec<f64> = lines
            .next()?
            .split_whitespace()
            .filter_map(|token| token.parse().ok())
            .collect();
        if geometry.len() < 4 {
            return None;
        }
        // İşaretten sonra: `q`, dört renk operatörü, `RG`, kalınlık, `w`.
        let colors: Vec<f64> = lines
            .nth(1)?
            .split_whitespace()
            .take(3)
            .filter_map(|token| token.parse().ok())
            .collect();
        if colors.len() < 3 {
            return None;
        }
        Some((
            (colors[0], colors[1], colors[2]),
            [geometry[0], geometry[1], geometry[2], geometry[3]],
        ))
    }

    #[test]
    fn code_box_is_a_tinted_rectangle_inside_the_text_column() {
        let page = PageSize::A4;
        let options = PdfOptions {
            theme: Theme::Light,
            page,
            ..Default::default()
        };
        let bytes = crate::pdf::render_article(&full_article(), &options, &Meta::default())
            .unwrap()
            .bytes;
        let text = content_streams(&bytes);
        let (fill, [_, y, width, height]) = code_box_fill(&text).expect("kod kutusu dolgusu yok");

        // Renk, temanın kod kutusu zemini olmalı.
        let want = Theme::Light.palette_with(CodeTheme::Auto).code.background;
        let (red, green, blue) = rgb(want);
        let want = (
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0,
        );
        assert!(
            (fill.0 - want.0).abs() < 0.01
                && (fill.1 - want.1).abs() < 0.01
                && (fill.2 - want.2).abs() < 0.01,
            "beklenen dolgu rengi {want:?}, bulunan {fill:?}"
        );

        // Kutu metin sütunu genişliğinde ve sayfanın içinde kalmalı.
        let (page_width_mm, page_height_mm) = page.dimensions_mm();
        assert!((width - page.content_width_mm() * MM_TO_PT).abs() < 0.5);
        assert!(width < page_width_mm * MM_TO_PT);
        assert!(height > 0.0 && height < page_height_mm * MM_TO_PT);
        assert!(y >= 0.0 && y + height <= page_height_mm * MM_TO_PT + 0.5);
    }

    /// Kod kutusu operatörlerindeki renk biçimi (dört ondalık).
    fn code_color_op(color: Color) -> String {
        let (red, green, blue) = rgb01(color);
        format!("{red:.4} {green:.4} {blue:.4} rg")
    }

    #[test]
    fn code_box_is_rounded_and_has_an_accent_stripe() {
        let bytes =
            crate::pdf::render_article(&full_article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let text = content_streams(&bytes);
        let palette = Theme::Light.palette_with(CodeTheme::Auto).code;
        let block = text
            .split(CODE_BOX_MARKER)
            .nth(1)
            .expect("kod kutusu bulunamadı");

        // Zemin ve çerçeve tek yolda basılır: dört yuvarlak köşe yayı (`c`)
        // ve `B` (dolgu + çizgi).
        let end = block.find('B').expect("yol kapatılmadı");
        let path = &block[..end];
        assert_eq!(
            path.matches(" c\n").count(),
            4,
            "yuvarlatılmış köşe yayları eksik: {path}"
        );
        assert!(path.contains("h\n"), "yol kapatılmalı (h)");
        assert!(
            path.contains(&code_color_op(palette.background)),
            "zemin rengi yok"
        );
        let (br, bg, bb) = rgb01(palette.border);
        assert!(
            path.contains(&format!("{br:.4} {bg:.4} {bb:.4} RG")),
            "çerçeve rengi yok"
        );

        // Sol şerit ayrı bir dolgudur ve kutunun sol yaylarını izler.
        assert!(text.contains(&code_color_op(palette.accent)), "şerit rengi yok");
        let stripe = block
            .split(&code_color_op(palette.accent))
            .nth(1)
            .expect("şerit yok");
        assert!(
            stripe[..stripe.find('f').unwrap_or(0)].matches(" c\n").count() >= 1,
            "şeridin sol köşeleri yuvarlatılmalı"
        );
    }

    /// Vurgulu kod satırı içeren makale (`full_article`ın kodu 3. satırda
    /// vurgulu).
    fn highlighted_article() -> Article {
        let mut article = full_article();
        for block in &mut article.blocks {
            if let Block::Code { highlights, .. } = block {
                *highlights = vec![2];
            }
        }
        article
    }

    /// `% snappdf:satir-vurgusu` bloğundan dolgu rengi ve dikdörtgen (pt).
    fn highlight_band(stream: &str) -> Option<((f64, f64, f64), [f64; 4])> {
        let after = stream.split(HIGHLIGHT_MARKER).nth(1)?;
        let mut lines = after.lines();
        let geometry: Vec<f64> = lines
            .next()?
            .split_whitespace()
            .filter_map(|token| token.parse().ok())
            .collect();
        let color_line = lines.find(|line| line.ends_with(" rg"))?;
        let colors: Vec<f64> = color_line
            .split_whitespace()
            .take(3)
            .filter_map(|token| token.parse().ok())
            .collect();
        if geometry.len() < 4 || colors.len() < 3 {
            return None;
        }
        Some((
            (colors[0], colors[1], colors[2]),
            [geometry[0], geometry[1], geometry[2], geometry[3]],
        ))
    }

    /// İçinde **ham** (sıkıştırılmamış) görsel akışı olan minik bir PDF kurar.
    fn pdf_with_raw_image(pixels: &[u8]) -> Vec<u8> {
        let mut doc = Document::new();
        let mut image = Dictionary::new();
        image.set("Type", "XObject");
        image.set("Subtype", "Image");
        image.set("Width", 64);
        image.set("Height", 64);
        image.set("BitsPerComponent", 8);
        image.set("ColorSpace", "DeviceRGB");
        let image_id = doc.add_object(lopdf::Stream::new(image, pixels.to_vec()));

        let content = b"q 64 0 0 64 0 0 cm /X0 Do Q".to_vec();
        let content_id = doc.add_object(lopdf::Stream::new(Dictionary::new(), content));
        let mut resources = Dictionary::new();
        let mut xobjects = Dictionary::new();
        xobjects.set("X0", Object::Reference(image_id));
        resources.set("XObject", xobjects);
        let mut page = Dictionary::new();
        page.set("Type", "Page");
        page.set("MediaBox", vec![0.into(), 0.into(), 595.into(), 842.into()]);
        page.set("Resources", resources);
        page.set("Contents", Object::Reference(content_id));
        let page_id = doc.add_object(page);
        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", vec![Object::Reference(page_id)]);
        pages.set("Count", 1);
        let pages_id = doc.add_object(pages);
        doc.get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Parent", Object::Reference(pages_id));
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    /// Bulunan görsel akışı (kimliği, akışı).
    fn image_stream(doc: &Document) -> (ObjectId, &lopdf::Stream) {
        doc.objects
            .iter()
            .find_map(|(id, object)| match object {
                Object::Stream(stream)
                    if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                        == Some(b"Image") =>
                {
                    Some((*id, stream))
                }
                _ => None,
            })
            .expect("görsel akışı bulunamadı")
    }

    #[test]
    fn raw_image_streams_are_compressed_losslessly() {
        // Ekran görüntüsü benzeri ham veri: tekrar eden pikseller.
        let pixels: Vec<u8> = (0..64 * 64)
            .flat_map(|i| [40u8, (i % 7) as u8 * 8, 200])
            .collect();
        let bytes = pdf_with_raw_image(&pixels);
        let before = bytes.len();
        let before_len = {
            let doc = Document::load_mem(&bytes).unwrap();
            image_stream(&doc).1.content.len()
        };
        assert_eq!(before_len, pixels.len(), "kaynak görsel ham olmalı");

        let out = apply(
            bytes,
            &OutputOptions {
                meta: Meta {
                    title: "Görsel".into(),
                    author: "test".into(),
                    language: "tr".into(),
                },
                page: PageSize::default(),
                background: None,
                code: None,
                code_boxes: vec![],
                code_highlights: vec![],
                code_decorations: vec![],
                bookmarks: vec![],
            },
        )
        .unwrap();

        let doc = Document::load_mem(&out).unwrap();
        let (_, image) = image_stream(&doc);
        assert_eq!(
            image
                .dict
                .get(b"Filter")
                .and_then(Object::as_name)
                .unwrap(),
            b"FlateDecode",
            "görsel akışı Flate ile sıkıştırılmalı"
        );
        // Kayıpsız: açılan veri birebir aynı olmalı. lopdf görsel akışlarını
        // çözmeyi reddeder (`decompressed_content` yalnızca /Subtype /Image
        // için hata verir), bu yüzden kopyadan etiket geçici olarak çıkarılır.
        let mut probe = image.clone();
        probe.dict.remove(b"Subtype");
        assert_eq!(probe.decompressed_content().unwrap(), pixels);
        assert!(out.len() < before, "dosya küçülmeli: {before} -> {}", out.len());
    }

    #[test]
    fn highlight_band_is_painted_between_the_box_and_the_code_text() {
        let page = PageSize::A4;
        let options = PdfOptions {
            theme: Theme::Light,
            page,
            ..Default::default()
        };
        let rendered =
            crate::pdf::render_article(&highlighted_article(), &options, &Meta::default()).unwrap();
        assert!(rendered.highlighted_lines >= 1, "vurgu yakalanmadı");
        let text = content_streams(&rendered.bytes);

        let (fill, band) = highlight_band(&text).expect("vurgu bandı çizilmedi");
        let (_, [box_x, box_y, box_w, box_h]) =
            code_box_fill(&text).expect("kod kutusu dolgusu yok");

        // Bant kutunun içinde: sol şeride ve çerçeveye değmez, satır
        // numaralarını da kapsar (iç dolgu kadar içeride).
        assert!(band[0] > box_x + 1.0, "{band:?} / {box_x}");
        assert!(band[0] + band[2] < box_x + box_w - 1.0, "{band:?}");
        assert!(band[1] > box_y, "{band:?} / {box_y}");
        assert!(band[1] + band[3] < box_y + box_h, "{band:?}");
        assert!(band[3] > 2.0 && band[3] < box_h, "satır yüksekliği: {band:?}");

        // Renk paletten gelmeli.
        let palette = Theme::Light.palette_with(CodeTheme::Auto).code;
        let (red, green, blue) = rgb01(palette.highlight);
        let want = (red, green, blue);
        assert!(
            (fill.0 - want.0).abs() < 0.01
                && (fill.1 - want.1).abs() < 0.01
                && (fill.2 - want.2).abs() < 0.01,
            "beklenen {want:?}, bulunan {fill:?}"
        );
        assert!(
            text.contains(&code_color_op(palette.highlight)),
            "vurgu rengi içerik akışında yok"
        );
    }

    #[test]
    fn highlight_band_stays_behind_the_code_text() {
        let rendered = crate::pdf::render_article(
            &highlighted_article(),
            &PdfOptions::default(),
            &Meta::default(),
        )
        .unwrap();
        let text = content_streams(&rendered.bytes);
        let kutu = text.find(CODE_BOX_MARKER).expect("kutu yok");
        let band = text.find(HIGHLIGHT_MARKER).expect("bant yok");
        let glyphs = text.find("TJ").expect("metin yok");
        assert!(kutu < band, "büyük dikdörtgen önce boyanmalı");
        assert!(band < glyphs, "bant kod metninin arkasında kalmalı");

        // Vurgu kapatıldığında akışta hiç bant kalmaz.
        let rendered = crate::pdf::render_article(
            &highlighted_article(),
            &PdfOptions {
                line_highlights: false,
                ..Default::default()
            },
            &Meta::default(),
        )
        .unwrap();
        assert!(!content_streams(&rendered.bytes).contains(HIGHLIGHT_MARKER));
        assert_eq!(rendered.highlighted_lines, 0);
    }

    #[test]
    fn rounded_rect_path_arcs_and_clamping() {
        let path = rounded_rect_path(0.0, 0.0, 100.0, 40.0, [2.0; 4]);
        assert_eq!(path.matches(" c\n").count(), 4);
        assert!(path.ends_with("h\n"));
        // Yayın bitiş noktasından başlanır (sol-alt + yarıçap).
        assert!(path.starts_with("2.00 0.00 m\n"), "{path}");

        // Keskin köşe: yay yerine düz çizgi.
        let sharp = rounded_rect_path(0.0, 0.0, 10.0, 10.0, [0.0; 4]);
        assert_eq!(sharp.matches(" c\n").count(), 0);
        assert_eq!(sharp.matches(" l\n").count(), 4);

        // Yarıçap kenarı aşarsa ölçeklenir (ince şerit).
        assert_eq!(fit_radii(100.0, 40.0, [2.0; 4]), [2.0; 4]);
        let shrunk = fit_radii(3.0, 100.0, [4.0, 0.0, 0.0, 4.0]);
        assert!((shrunk[0] - 3.0).abs() < 1e-6, "{shrunk:?}");
        assert!((shrunk[3] - 3.0).abs() < 1e-6, "{shrunk:?}");
        // Yarıçapsız dikdörtgen etkilenmez.
        assert_eq!(fit_radii(10.0, 10.0, [0.0; 4]), [0.0; 4]);
    }

    #[test]
    fn code_box_is_painted_on_top_of_the_page_background() {
        let options = PdfOptions {
            theme: Theme::Dark,
            ..Default::default()
        };
        let bytes = crate::pdf::render_article(&full_article(), &options, &Meta::default())
            .unwrap()
            .bytes;
        let text = content_streams(&bytes);
        let background = text.find(PAGE_BACKGROUND_MARKER).expect("sayfa zemini yok");
        let code = text.find(CODE_BOX_MARKER).expect("kod kutusu zemini yok");
        assert!(
            background < code,
            "kod kutusu sayfa zemininden önce çizilmiş (zemin kutuyu örterdi)"
        );
    }

    #[test]
    fn code_syntax_colors_reach_the_pdf() {
        let bytes =
            crate::pdf::render_article(&full_article(), &PdfOptions::default(), &Meta::default())
                .unwrap()
                .bytes;
        let text = content_streams(&bytes);
        let palette = Theme::Light.palette_with(CodeTheme::Auto).code;
        for (label, color) in [
            ("anahtar sözcük", palette.keyword),
            ("işlev", palette.function),
            ("dize", palette.string),
            ("yorum", palette.comment),
        ] {
            assert!(prints_color(&text, color), "{label} rengi basılmadı");
        }
    }

    #[test]
    fn code_box_uses_the_selected_code_theme() {
        let options = PdfOptions {
            theme: Theme::Light,
            code_theme: CodeTheme::Monokai,
            ..Default::default()
        };
        let bytes = crate::pdf::render_article(&full_article(), &options, &Meta::default())
            .unwrap()
            .bytes;
        let (fill, _) = code_box_fill(&content_streams(&bytes)).expect("kod kutusu dolgusu yok");
        let (red, green, blue) = rgb(CodeTheme::Monokai.palette(Theme::Light).background);
        let want = (
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0,
        );
        assert!(
            (fill.0 - want.0).abs() < 0.01
                && (fill.1 - want.1).abs() < 0.01
                && (fill.2 - want.2).abs() < 0.01,
            "monokai zemini bekleniyordu: {want:?}, bulunan {fill:?}"
        );
    }

    #[test]
    fn code_themes_never_print_default_black_text() {
        // Satır numarası, ayraç, sarma işareti ve rozet de renk almalı; aksi
        // hâlde koyu kod zeminlerinde okunmaz olurlar.
        for code_theme in [
            CodeTheme::GithubLight,
            CodeTheme::GithubDark,
            CodeTheme::Monokai,
            CodeTheme::SolarizedLight,
            CodeTheme::SolarizedDark,
            CodeTheme::Sepia,
        ] {
            for theme in [Theme::Light, Theme::Dark, Theme::Sepia] {
                let options = PdfOptions {
                    theme,
                    code_theme,
                    ..Default::default()
                };
                let bytes = crate::pdf::render_article(&full_article(), &options, &Meta::default())
                    .unwrap()
                    .bytes;
                let colors = text_fill_colors(&content_streams(&bytes));
                let black = colors
                    .iter()
                    .filter(|(r, g, b)| *r == 0.0 && *g == 0.0 && *b == 0.0)
                    .count();
                assert_eq!(
                    black, 0,
                    "{theme:?}/{code_theme:?}: {black} metin varsayılan siyahla basılmış"
                );
            }
        }
    }

    #[test]
    fn apply_without_work_returns_input_untouched() {
        let bytes = b"%PDF-1.4 minimal".to_vec();
        let out = apply(
            bytes.clone(),
            &OutputOptions {
                meta: Meta::default(),
                page: PageSize::default(),
                background: None,
                code: None,
                code_boxes: vec![],
                code_highlights: vec![],
                code_decorations: vec![],
                bookmarks: vec![],
            },
        )
        .unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn meta_equality_distinguishes_fields() {
        assert_eq!(Meta::default(), Meta::default());
        assert_ne!(
            Meta::default(),
            Meta {
                language: "tr".into(),
                ..Meta::default()
            }
        );
    }

    #[test]
    fn destination_uses_page_top_coordinates() {
        let mut pages = BTreeMap::new();
        pages.insert(1u32, (7u32, 0u16));
        // Sayfa üstünden 29,7 mm (A4'ün %10'u) aşağıda bir hedef.
        let object = destination(&bookmark("x", 1, 1, 29.7), &pages, PageSize::A4);
        let array = object.as_array().unwrap();
        let top = array[3].as_f64().unwrap();
        let expected = (297.0 - 29.7) * MM_TO_PT;
        assert!(
            (top - expected).abs() < 0.01,
            "üst: {top} beklenen: {expected}"
        );
    }

    #[test]
    fn rendered_bookmarks_point_at_existing_pages() {
        let (bytes, bookmarks) = rendered(&PdfOptions::default());
        assert!(bytes.starts_with(b"%PDF"));
        assert_eq!(bookmarks.len(), 4, "beklenen yer imi sayısı");
        assert_eq!(bookmarks[0].title, "Yer imli belge");
        assert_eq!(bookmarks[0].level, 0);
        assert_eq!(bookmarks[1].title, "Birinci bölüm");
        assert_eq!(bookmarks[1].level, 2);
        let doc = Document::load_mem(&bytes).unwrap();
        let page_count = doc.get_pages().len() as u32;
        for bookmark in &bookmarks {
            assert!(bookmark.page >= 1 && bookmark.page <= page_count);
            assert!(bookmark.y_mm >= 0.0);
        }
    }

    #[test]
    fn bookmarks_can_be_disabled() {
        let (bytes, bookmarks) = rendered(&PdfOptions {
            bookmarks: false,
            ..Default::default()
        });
        assert!(bookmarks.is_empty());
        // Yer imi kapalıyken /Outlines boş kalır (printpdf'in bıraktığı hâli).
        let doc = Document::load_mem(&bytes).unwrap();
        let catalog = doc.catalog().unwrap();
        let root = catalog.get(b"Outlines").unwrap().as_reference().unwrap();
        let root = doc.get_dictionary(root).unwrap();
        assert!(root.get(b"First").is_err());
    }
}
