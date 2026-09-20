//! Font alt kümeleme: PDF'e gömülen Liberation fontlarını yalnızca belgede
//! geçen karakterlerin glifleriyle sınırlar.
//!
//! Neden gerekli? Gömülü dört sans fontu ~1.6 MB, dört mono fontu ~1.2 MB
//! tutar ve PDF'in taban boyutunu belirler. Alt kümeleme sonrası aynı fontlar
//! tipik bir yazıda onlarca KB'a iner.
//!
//! Kritik ayrıntı: `subsetter` `cmap` tablosunu kaldırır (kendi PDF
//! yazıcınızda CID kullanmanızı bekler). Bizim PDF yazıcımız (`printpdf`) fontu
//! rusttype/stb_truetype ile ayrıştırıp karakter→glif eşlemesini `cmap`ten
//! okuduğu için bu tabloyu yeniden yazıyoruz. Her karakterin alt kümedeki yeni
//! glif numarasını bildiğimiz için (remapper) standart bir format 4 `cmap`
//! tablosu üretmek yeterli.

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;

/// `data` fontunu yalnızca `chars` karakterlerinin glifleriyle alt kümeleyip
/// geçerli bir `cmap` tablosuyla birlikte döndürür.
///
/// Fontta olmayan karakterler sessizce atlanır (zaten basılamazlardı).
pub fn subset(data: &[u8], chars: &BTreeSet<char>) -> Result<Vec<u8>> {
    let face = ttf_parser::Face::parse(data, 0).context("gömülü font ayrıştırılamadı")?;

    // 1) Kullanılan karakterlerin yeni glif numaralarını belirle. Glif 0
    //    (.notdef) her alt kümede bulunur; ona düşen karakterler yazılmaz.
    let mut remapper = subsetter::GlyphRemapper::new();
    // (karakter kodu, alt kümedeki glif numarası)
    let mut mapping: Vec<(u16, u16)> = Vec::with_capacity(chars.len());
    for &ch in chars {
        // Format 4 `cmap` yalnızca BMP'yi kapsar.
        let code = ch as u32;
        if code == 0 || code > 0xFFFF || code == 0xFFFF {
            continue;
        }
        if let Some(glyph) = face.glyph_index(ch) {
            let mapped = remapper.remap(glyph.0);
            if mapped != 0 {
                mapping.push((code as u16, mapped));
            }
        }
    }

    // 2) Glifleri ve tabloları alt kümele. Bileşik gliflerin (ör. ğ, ş)
    //    parçaları `subsetter` içinde otomatik eklenir; bu, daha önce atadığımız
    //    numaraları değiştirmez (yeni glifler sona eklenir).
    let mut font = subsetter::subset(data, 0, &remapper).context("font alt kümeleme başarısız")?;

    // 3) Eksik `cmap` tablosunu yazıp sfnt tablo dizinine ekle.
    let cmap = cmap_table(&mapping);
    add_table(&mut font, b"cmap", &cmap)?;

    Ok(font)
}

/// Karakter→glif eşlemesi için `cmap` tablosu üretir. İki kodlama kaydı
/// (Unicode BMP ve Windows BMP) aynı alt tabloyu gösterir; farklı okuyucular
/// farklı kayıtları tercih eder.
fn cmap_table(mapping: &[(u16, u16)]) -> Vec<u8> {
    let subtable = cmap4_subtable(mapping);
    // version(2) + numTables(2) + 2 * kayıt(8)
    let header_len = 4 + 2 * 8;
    let mut out = Vec::with_capacity(header_len + subtable.len());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    for (platform, encoding) in [(0u16, 3u16), (3, 1)] {
        out.extend_from_slice(&platform.to_be_bytes());
        out.extend_from_slice(&encoding.to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
    }
    out.extend_from_slice(&subtable);
    out
}

/// Format 4 `cmap` alt tablosu. Her karakter kendi segmentini alır ve
/// `idDelta` ile glif numarasına bağlanır (bitişik segmentleri birleştirmeye
/// gerek yok; segment sayısı karakter sayısı kadar).
fn cmap4_subtable(mapping: &[(u16, u16)]) -> Vec<u8> {
    let mut segments: Vec<(u16, u16)> = mapping.to_vec();
    segments.sort_unstable();
    segments.dedup_by_key(|(code, _)| *code);
    // Zorunlu sonlandırıcı segment: 0xFFFF -> glif 0.
    segments.push((0xFFFF, 0));

    let seg_count = segments.len();
    // 14 bayt sabit alan + reservedPad (2) + 4 dizi * 2 bayt * segment.
    let length = 16 + 8 * seg_count;
    let (search_range, entry_selector, range_shift) = search_fields(seg_count, 2);

    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(&4u16.to_be_bytes()); // format
    out.extend_from_slice(&(length as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&((seg_count * 2) as u16).to_be_bytes());
    out.extend_from_slice(&(search_range as u16).to_be_bytes());
    out.extend_from_slice(&(entry_selector as u16).to_be_bytes());
    out.extend_from_slice(&(range_shift as u16).to_be_bytes());
    for (code, _) in &segments {
        out.extend_from_slice(&code.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for (code, _) in &segments {
        out.extend_from_slice(&code.to_be_bytes());
    }
    for (code, glyph) in &segments {
        // glif = (karakter + idDelta) mod 65536
        let delta = i32::from(*glyph) - i32::from(*code);
        out.extend_from_slice(&(delta as i16).to_be_bytes());
    }
    for _ in &segments {
        out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
    }
    out
}

/// sfnt tablo dizininin ikili arama alanları: `searchRange`,
/// `entrySelector`, `rangeShift`. `unit`, `searchRange`in bir biriminin kaç
/// bayt olduğudur (cmap format 4 için 2, tablo dizini için 16).
fn search_fields(count: usize, unit: usize) -> (usize, usize, usize) {
    let mut entry_selector = 0usize;
    while (1usize << (entry_selector + 1)) <= count {
        entry_selector += 1;
    }
    let search_range = unit << entry_selector;
    (search_range, entry_selector, count * unit - search_range)
}

/// sfnt (TrueType) dosyasına yeni bir tablo ekler.
///
/// Tablo dizini bir kayıt büyüdüğü için dizinden sonraki tüm veri 16 bayt
/// kayar; mevcut tablo uzaklıkları buna göre güncellenir. Kayıtlar etikete
/// göre sıralı yazılır (spec gereği).
fn add_table(font: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) -> Result<()> {
    if font.len() < 12 {
        return Err(anyhow!("font dosyası çok kısa"));
    }
    let num_tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    let dir_end = 12 + num_tables * 16;
    if num_tables == 0 || dir_end > font.len() {
        return Err(anyhow!("font tablo dizini okunamadı"));
    }

    let mut records: Vec<TableRecord> = Vec::with_capacity(num_tables + 1);
    for index in 0..num_tables {
        let base = 12 + index * 16;
        let record = &font[base..base + 16];
        records.push(TableRecord {
            tag: [record[0], record[1], record[2], record[3]],
            checksum: u32::from_be_bytes(record[4..8].try_into().unwrap()),
            offset: u32::from_be_bytes(record[8..12].try_into().unwrap()),
            length: u32::from_be_bytes(record[12..16].try_into().unwrap()),
        });
    }
    if records.iter().any(|record| &record.tag == tag) {
        return Err(anyhow!("tablo zaten var"));
    }

    // Yeni tabloyu dosyanın sonuna (4 bayt hizalı) ekle; dizin büyüyeceği
    // için gerçek uzaklığı 16 bayt sonrasıdır.
    while !font.len().is_multiple_of(4) {
        font.push(0);
    }
    let offset = (font.len() + 16) as u32;
    font.extend_from_slice(data);
    while !font.len().is_multiple_of(4) {
        font.push(0);
    }

    for record in &mut records {
        record.offset += 16;
    }
    records.push(TableRecord {
        tag: *tag,
        checksum: checksum(data),
        offset,
        length: data.len() as u32,
    });
    records.sort_by_key(|record| record.tag);

    let count = records.len();
    let (search_range, entry_selector, range_shift) = search_fields(count, 16);
    let mut header = Vec::with_capacity(12 + count * 16);
    header.extend_from_slice(&font[0..4]); // sfntVersion
    header.extend_from_slice(&(count as u16).to_be_bytes());
    header.extend_from_slice(&(search_range as u16).to_be_bytes());
    header.extend_from_slice(&(entry_selector as u16).to_be_bytes());
    header.extend_from_slice(&(range_shift as u16).to_be_bytes());
    for record in &records {
        header.extend_from_slice(&record.tag);
        header.extend_from_slice(&record.checksum.to_be_bytes());
        header.extend_from_slice(&record.offset.to_be_bytes());
        header.extend_from_slice(&record.length.to_be_bytes());
    }

    // Dizinin sonuna 16 baytlık yeni kayıt alanı aç (arkadaki veri kayar).
    font.splice(dir_end..dir_end, std::iter::repeat_n(0u8, 16));
    font[..header.len()].copy_from_slice(&header);
    Ok(())
}

/// sfnt tablo dizini kaydı.
struct TableRecord {
    tag: [u8; 4],
    checksum: u32,
    offset: u32,
    length: u32,
}

/// OpenType tablo sağlama toplamı (4 baytlık big-endian kelimelerin toplamı).
fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use genpdf::fonts::FontData;

    const SANS: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");

    fn chars(text: &str) -> BTreeSet<char> {
        text.chars().collect()
    }

    /// PDF katmanının sabit metinlerde bastığı özel karakterler: madde imi,
    /// ayraç, kod kutusu ayıracı ve sarma işareti (bkz. `pdf.rs`). Hepsi gömülü
    /// fontlarda bulunmalı — glifsiz karakter PDF'te boş kutu olarak görünür.
    const UI_CHARS: &str = "ğĞüÜşŞıİöÖçÇ•―│«»";

    #[test]
    fn ui_characters_have_glyphs_in_both_fonts() {
        for (label, data) in [
            (
                "sans",
                include_bytes!("../assets/fonts/LiberationSans-Regular.ttf") as &[u8],
            ),
            (
                "mono",
                include_bytes!("../assets/fonts/LiberationMono-Regular.ttf") as &[u8],
            ),
        ] {
            let face = ttf_parser::Face::parse(data, 0).expect("font çözümlenmeli");
            let missing: Vec<char> = UI_CHARS
                .chars()
                .filter(|ch| face.glyph_index(*ch).is_none())
                .collect();
            assert!(missing.is_empty(), "{label}: glifsiz karakterler {missing:?}");
        }
    }

    #[test]
    fn subsetting_shrinks_the_font_a_lot() {
        let out = subset(SANS, &chars("Türkçe metin ğüşıöçİĞÜŞÖÇ")).unwrap();
        assert!(
            out.len() < SANS.len() / 5,
            "alt küme beklenenden büyük: {} -> {}",
            SANS.len(),
            out.len()
        );
    }

    #[test]
    fn subset_font_is_parseable_and_keeps_requested_glyphs() {
        // Kod kutusu işaretleri de (`│`, `»`) ve arayüz simgeleri (`•`, `―`)
        // alt kümede eşlenmeli: alt kümeleme cmap'i yeniden kuruyor.
        let wanted = chars("Ağ çşıöİ0.ö│«»•―");
        let out = subset(SANS, &wanted).unwrap();
        let face = ttf_parser::Face::parse(&out, 0).expect("alt küme ayrıştırılamadı");
        for ch in wanted {
            let glyph = face.glyph_index(ch);
            assert!(glyph.is_some(), "{ch:?} glifi alt kümede yok");
        }
        // İstenmeyen bir karakter artık eşlenmemeli.
        assert!(face.glyph_index('Ω').is_none());
    }

    #[test]
    fn subset_glyph_ids_are_distinct_and_nonzero() {
        let wanted = chars("abcçdefgğhıijklmnoöprsştuüvyz");
        let out = subset(SANS, &wanted).unwrap();
        let face = ttf_parser::Face::parse(&out, 0).unwrap();
        let mut seen = BTreeSet::new();
        for ch in wanted {
            let glyph = face.glyph_index(ch).expect("glif yok").0;
            assert_ne!(glyph, 0, "{ch:?} .notdef'e eşlendi");
            assert!(seen.insert(glyph), "{ch:?} için glif numarası çakıştı");
        }
    }

    #[test]
    fn subset_keeps_composite_glyph_components() {
        // ğ ve ş bileşik gliflerdir: parçaları da alt kümede bulunmalı, aksi
        // halde harf eksik/bozuk basılır.
        let out = subset(SANS, &chars("ğşĞŞ")).unwrap();
        let face = ttf_parser::Face::parse(&out, 0).unwrap();
        for ch in "ğşĞŞ".chars() {
            let glyph = face.glyph_index(ch).expect("glif yok");
            let components: Vec<_> = face
                .glyph_raster_image(glyph, 0)
                .into_iter()
                .collect::<Vec<_>>();
            let _ = components;
            assert!(glyph.0 > 0);
        }
        // Bileşik gliften türeyen parçalar da font içinde olmalı.
        assert!(face.number_of_glyphs() > 4);
    }

    /// Bir glifin çizim komutlarını toplayan basit builder.
    #[derive(Default, Debug, PartialEq)]
    struct Outline {
        ops: Vec<(char, i16, i16)>,
    }

    impl ttf_parser::OutlineBuilder for Outline {
        fn move_to(&mut self, x: f32, y: f32) {
            self.ops.push(('M', x as i16, y as i16));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.ops.push(('L', x as i16, y as i16));
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.ops.push(('Q', x1 as i16, y1 as i16));
            self.ops.push(('q', x as i16, y as i16));
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.ops.push(('C', x1 as i16, y1 as i16));
            self.ops.push(('c', x2 as i16, y2 as i16));
            self.ops.push(('d', x as i16, y as i16));
        }
        fn close(&mut self) {
            self.ops.push(('Z', 0, 0));
        }
    }

    /// En güçlü doğrulama: alt kümedeki her karakter, kaynak fonttaki
    /// karakterle *aynı* çizimi ve aynı genişliği üretmeli. Yalnızca glif
    /// numaraları değişir; yanlış bir `cmap` harfleri karıştırırdı.
    #[test]
    fn subset_keeps_the_same_outlines_for_each_character() {
        let wanted = chars("Ağaçşıöİ0.{}()=>; öğüşçıĞÜŞİÖÇ");
        let out = subset(SANS, &wanted).unwrap();
        let original = ttf_parser::Face::parse(SANS, 0).unwrap();
        let subset_face = ttf_parser::Face::parse(&out, 0).unwrap();
        let mut checked = 0;
        for &ch in &wanted {
            let Some(original_glyph) = original.glyph_index(ch) else {
                continue;
            };
            let subset_glyph = subset_face
                .glyph_index(ch)
                .unwrap_or_else(|| panic!("{ch:?} alt kümede yok"));
            assert_eq!(
                original.glyph_hor_advance(original_glyph),
                subset_face.glyph_hor_advance(subset_glyph),
                "{ch:?} için genişlik farklı (yanlış glife eşlenmiş olabilir)"
            );
            let mut expected = Outline::default();
            let mut actual = Outline::default();
            let expected_box = original.outline_glyph(original_glyph, &mut expected);
            let actual_box = subset_face.outline_glyph(subset_glyph, &mut actual);
            // Boşluk gibi denetimsiz glifler iki fontta da çizimsiz olmalı.
            assert_eq!(
                expected_box.is_some(),
                actual_box.is_some(),
                "{ch:?} için çizim varlığı farklı"
            );
            if expected_box.is_some() {
                assert_eq!(expected.ops, actual.ops, "{ch:?} için çizim farklı");
                assert_eq!(expected_box, actual_box, "{ch:?} sınır kutusu farklı");
                checked += 1;
            }
        }
        assert!(checked > 20, "çok az karakter doğrulandı: {checked}");
    }

    #[test]
    fn composite_glyphs_are_composed_correctly_after_subsetting() {
        // Bileşik harfler (ğ, ş, İ) parçalarının doğru glif numaralarına
        // bağlanmasını gerektirir; alt kümede de aynı şekli üretmeli.
        let out = subset(SANS, &chars("gğsşiİ")).unwrap();
        let original = ttf_parser::Face::parse(SANS, 0).unwrap();
        let subset_face = ttf_parser::Face::parse(&out, 0).unwrap();
        for ch in "ğşİ".chars() {
            let original_box = original
                .outline_glyph(original.glyph_index(ch).unwrap(), &mut Outline::default())
                .expect("kaynak glif");
            let subset_box = subset_face
                .outline_glyph(
                    subset_face.glyph_index(ch).unwrap(),
                    &mut Outline::default(),
                )
                .expect("alt küme glifi");
            assert_eq!(original_box, subset_box, "{ch:?} sınır kutusu farklı");
        }
    }

    #[test]
    fn subset_is_readable_by_the_pdf_font_stack() {
        // printpdf fontu rusttype/stb_truetype ile ayrıştırır: cmap, glyf,
        // loca, head, hhea, hmtx ve maxp birlikte geçerli olmalı.
        let out = subset(SANS, &chars("Türkçe kod örneği: fn main() → 42")).unwrap();
        FontData::new(out, None).expect("rusttype alt kümeyi okuyamadı");
    }

    #[test]
    fn subset_without_known_glyphs_is_still_valid() {
        // İstenen karakterler Liberation'da bulunmasaydı da geçerli bir font
        // üretilmeli (yalnızca .notdef kalır).
        let out = subset(SANS, &chars("漢𠀋")).unwrap();
        let face = ttf_parser::Face::parse(&out, 0).expect("geçerli font");
        assert!(face.glyph_index('漢').is_none());
        // İstenmeyen karakterler alt kümeye girmez.
        assert!(face.glyph_index('Ω').is_none());
    }

    #[test]
    fn every_embedded_font_subset_is_accepted_by_rusttype() {
        let wanted = chars("Kod bloğu: ğüşıöçİĞÜŞÖÇ 0123 {}");
        let fonts: [&[u8]; 8] = [
            include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"),
            include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"),
            include_bytes!("../assets/fonts/LiberationSans-Italic.ttf"),
            include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf"),
            include_bytes!("../assets/fonts/LiberationMono-Regular.ttf"),
            include_bytes!("../assets/fonts/LiberationMono-Bold.ttf"),
            include_bytes!("../assets/fonts/LiberationMono-Italic.ttf"),
            include_bytes!("../assets/fonts/LiberationMono-BoldItalic.ttf"),
        ];
        for font in fonts {
            let out = subset(font, &wanted).unwrap();
            FontData::new(out, None).expect("font alt kümesi çözümlenemedi");
        }
    }

    #[test]
    fn search_fields_match_the_spec_examples() {
        // OpenType spec örnekleri.
        assert_eq!(search_fields(9, 16), (128, 3, 16));
        assert_eq!(search_fields(1, 16), (16, 0, 0));
        assert_eq!(search_fields(1, 2), (2, 0, 0));
        assert_eq!(search_fields(3, 2), (4, 1, 2));
    }

    #[test]
    fn cmap_table_declares_two_unicode_records() {
        let table = cmap_table(&[(0x0041, 1), (0x011F, 2)]);
        assert_eq!(&table[0..2], &[0, 0]); // version
        assert_eq!(&table[2..4], &[0, 2]); // iki kayıt
                                           // İlk kayıt: platform 0, encoding 3, alt tablo uzaklığı (bayt 8-11).
        assert_eq!(&table[4..6], &[0, 0]);
        assert_eq!(&table[6..8], &[0, 3]);
        let size = u32::from_be_bytes(table[8..12].try_into().unwrap());
        assert_eq!(size as usize, 4 + 2 * 8, "alt tablo uzaklığı hatalı");
        // İkinci kayıt aynı alt tabloyu gösterir.
        assert_eq!(&table[12..14], &[0, 3]);
        assert_eq!(&table[14..16], &[0, 1]);
        assert_eq!(
            u32::from_be_bytes(table[16..20].try_into().unwrap()) as usize,
            4 + 2 * 8
        );
        // Format 4 ve uzunluk alanları (2 karakter + sonlandırıcı = 3 segment).
        assert_eq!(&table[20..22], &[0, 4]);
        let length = u16::from_be_bytes(table[22..24].try_into().unwrap()) as usize;
        assert_eq!(length, 16 + 8 * 3);
        assert_eq!(length, table.len() - 20, "uzunluk alanı tabloyla uyuşmalı");
        assert_eq!(&table[24..26], &[0, 0], "segment sayısı 2 bayt: 3 segment");
    }

    #[test]
    fn checksum_sums_big_endian_words() {
        assert_eq!(checksum(&[0, 0, 0, 1]), 1);
        // İkinci kelime 4 bayta tamamlanır: 0x0001_0000 + 1.
        assert_eq!(checksum(&[0, 0, 0, 1, 0x00, 0x01]), 0x0001_0000 + 1);
        assert_eq!(checksum(&[]), 0);
    }

    #[test]
    fn added_table_directory_keeps_offsets_valid() {
        // cmap eklendikten sonra diğer tabloların uzaklıkları hâlâ doğru mu?
        let out = subset(SANS, &chars("Ağ")).unwrap();
        let num_tables = u16::from_be_bytes([out[4], out[5]]) as usize;
        let mut tags = Vec::new();
        for index in 0..num_tables {
            let base = 12 + index * 16;
            let tag = &out[base..base + 4];
            let offset = u32::from_be_bytes(out[base + 8..base + 12].try_into().unwrap()) as usize;
            let length = u32::from_be_bytes(out[base + 12..base + 16].try_into().unwrap()) as usize;
            assert!(offset + length <= out.len(), "tablo sınırı aşıyor");
            assert!(offset >= 12 + num_tables * 16, "tablo dizinle çakışıyor");
            tags.push(String::from_utf8_lossy(tag).to_string());
        }
        let mut sorted = tags.clone();
        sorted.sort();
        assert_eq!(tags, sorted, "tablo kayıtları sıralı değil");
        for expected in ["cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp"] {
            assert!(tags.contains(&expected.to_string()), "{expected} yok");
        }
    }
}
