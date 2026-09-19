# snappdf 📄⚡

**Link → güzel PDF.** Verdiğin URL'leri temiz, düzenli ve **reklamsız** PDF'lere
çevirir — **Chrome ya da herhangi bir tarayıcı gerekmez.** Tek komut, tek
ikili, tamamen çevrimdışı çalışan PDF motoru.

## Nasıl çalışıyor?

1. **HTTP indirme** (`reqwest`): sayfa 3x yönlendirmelerle indirilir; HTML
   olmayan içerik reddedilir, geçici hatalarda otomatik tekrar denenir
2. **Adblock motoru** (Brave'ın `adblock-rust`'u + EasyList/EasyPrivacy):
   makaledeki reklam/izleyici görsel URL'leri elenir
3. **İçerik çıkarımı** (`scraper`, readability benzeri skorlama): makale
   gövdesi (`<article>`, `<main>`, `[role=main]`…) seçilir; başlık,
   paragraf, kod bloğu, alıntı, liste, görsel ve ayraçlar sırayla toplanır
4. **Saf Rust PDF üretimi** (`genpdf`): A4, sayfa numaralı altbilgi,
   gömülü **Liberation** fontları → Türkçe karakterler (ğüşıöçİ) sorunsuz
5. Görseller (PNG/JPEG) indirilip PDF'e gömülür; yüklenemeyenler zarif
   notla değiştirilir

## Kurulum

```bash
cargo install --path .
```

`snappdf: komut bulunamıyor` hatası alırsan:

```bash
snappdf --install   # kabuk profiline PATH'i otomatik ekler
snappdf --doctor    # kurulumu doğrula
```

## Kullanım

```bash
# Tek link
snappdf https://www.rust-lang.org/

# Birden fazla link, çıktı klasörü belirterek
snappdf https://site.com/makale-1 https://site.com/dokuman -o ~/Documents/pdf

# İnce ayar
snappdf <url> --no-images     # görselleri PDF'e gömme
snappdf <url> --no-footer     # altbilgiyi (başlık + sayfa no) kapat
snappdf --refresh-filters     # filtre listelerini yeniden indir
```

Çıktı dosyası adı host'tan türetilir: `developer.mozilla.org.pdf`,
`www.rust-lang.org.pdf` ...

Gereksinimler:
- İlk çalıştırmada filtre listeleri (~2 MB) indirilir, 7 gün önbellekte tutulur
  (macOS: `~/Library/Caches/snappdf`, Linux: `~/.cache/snappdf`,
  Windows: `%LOCALAPPDATA%\snappdf`)

## İpuçları

- **SPA/JS ağırlıklı siteler**: snappdf statik HTML'i işler; JS ile
  sonradan basılan içerik görünmez (tarayıcı olmadığı için bilinçli tercih)
- **"makale gövdesi bulunamadı"**: sayfada `<article>`/`<main>` benzeri
  kapsayıcı yok demektir; site ana sayfaları böyledir — doğrudan yazı
  URL'si deneyin
- Yeni banner/kural gerekirse `src/extract.rs` içindeki `skip` ve aday
  seçici listeleri 2 satırlık düzenlemeyle genişletilebilir

## Mimari

```
src/
├── main.rs     CLI + orkestrasyon
├── fetch.rs    HTTP indirme (retry, içerik-tipi doğrulama, latin-1 fallback)
├── lists.rs    EasyList/EasyPrivacy indirme + önbellek
├── blocker.rs  adblock-rust Engine sarmalayıcı (görsel süzme)
├── extract.rs  HTML -> makale blokları (readability benzeri skorlama)
├── images.rs   Görsel indirme/çözümleme/önbellek
├── pdf.rs      genpdf ile A4 PDF (gömülü fontlar, altbilgi)
└── install.rs  --install (PATH kurulumu) + --doctor (teşhis)
```

Fontlar (`assets/fonts/`, SIL OFL 1.1) binary'ye gömülüdür: harici font
dosyası gerekmez, çıktı her makinede aynı görünür.

## Testler

```bash
cargo test        # 77 test
cargo clippy      # uyarısız
```

Yerel HTTP test sunucusuyla ağ yolları, içerik çıkarımı, PDF üretimi,
görsel gömme ve kurulum akışının tamamı kapsanır.
