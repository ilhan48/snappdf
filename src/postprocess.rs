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

use crate::pdf::{Bookmark, PageSize};
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
    /// Yer imleri (belge sırasında).
    pub bookmarks: Vec<Bookmark>,
}

/// Üretilmiş PDF'e meta veri, yer imi ağacı ve tema zeminini uygular.
pub fn apply(bytes: Vec<u8>, options: &OutputOptions) -> Result<Vec<u8>> {
    if options.bookmarks.is_empty()
        && options.background.is_none()
        && options.meta == Meta::default()
    {
        return Ok(bytes);
    }

    let mut doc = Document::load_mem(&bytes).context("üretilen PDF yeniden okunamadı")?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Ok(bytes);
    }

    if options.background.is_some() {
        paint_background(&mut doc, &pages, options)?;
    }
    set_metadata(&mut doc, &options.meta)?;
    if !options.bookmarks.is_empty() {
        if let Some(root) = write_outlines(&mut doc, &options.bookmarks, &pages, options.page) {
            attach_outlines(&mut doc, root)?;
        }
    }

    let mut out = Vec::with_capacity(bytes.len() + 2048);
    doc.save_to(&mut out).context("PDF sonlandırılamadı")?;
    Ok(out)
}

// ------------------------------------------------------------------- zemin

/// Her sayfanın içerik akışının başına tam sayfa dolgu ekler.
///
/// Zemin, içeriğin *arkasında* kalmalı; bu yüzden operatörler akışın başına
/// eklenir. Kenar boşlukları da kaplandığı için dolgu sayfa ölçüsündedir.
fn paint_background(
    doc: &mut Document,
    pages: &BTreeMap<u32, ObjectId>,
    options: &OutputOptions,
) -> Result<()> {
    let Some(color) = options.background else {
        return Ok(());
    };
    let (red, green, blue) = rgb(color);
    let (width_mm, height_mm) = options.page.dimensions_mm();
    let operators = format!(
        "q {:.4} {:.4} {:.4} rg 0 0 {:.2} {:.2} re f Q\n",
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
        width_mm * MM_TO_PT,
        height_mm * MM_TO_PT
    );

    for page_id in pages.values() {
        let contents = doc.get_page_contents(*page_id);
        if contents.is_empty() {
            // İçeriksiz sayfa (olmaması beklenir): zemin için akış oluştur.
            let mut stream = lopdf::Stream::new(Dictionary::new(), operators.clone().into_bytes());
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
            content.splice(0..0, operators.bytes());
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(content);
            stream.compress().context("sayfa içeriği sıkıştırılamadı")?;
        }
    }
    Ok(())
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
    use crate::pdf::{PdfOptions, Theme};

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
            text.starts_with("q ") && text.contains(" re f Q"),
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
        assert!(!text.contains(" re f Q"));
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
                Block::Code("fn main() {}\n    println!(\"x\");".into()),
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
            // Açık temada zemin çizilmez.
            assert!(
                !text.contains(" re f Q"),
                "{page:?}: beklenmeyen zemin dolgusu"
            );
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
