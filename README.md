# snappdf 📄⚡

**Link → güzel PDF.** Verdiğin URL'leri temiz, düzenli ve **reklamsız** PDF'lere
çevirir — **Chrome ya da herhangi bir tarayıcı gerekmez.** Tek komut, tek
ikili, tamamen çevrimdışı çalışan PDF motoru.

## Nasıl çalışıyor?

1. **HTTP indirme** (`reqwest`): sayfa 3x yönlendirmelerle indirilir; HTML
   olmayan içerik reddedilir, geçici hatalarda otomatik tekrar denenir
2. **Adblock motoru** (Brave'ın `adblock-rust`'u + EasyList/EasyPrivacy):
   makaledeki reklam/izleyici görselleri elenir
3. **İçerik çıkarımı** (`scraper` + `ego-tree`, readability benzeri):
   - Makale gövdesi **skorlanarak** seçilir. Skor hesaplanırken reklam, yan
     içerik, "ilgili yazılar", yorum formu gibi **boilerplate alt ağaçlar
     tamamen atlanır**; en yüksek skorlu aday genelde makaleyi saran geniş bir
     kapsayıcı olduğu için, skora **%90 yakın olan adaylar arasından en küçüğü**
     seçilir. Böylece sayfa başlığındaki kategori/etiket satırları ve benzeri
     arayüz parçaları PDF'e sızmaz.
   - Bilinen şablon seçicileri (WordPress, Ghost, Medium, Substack, Hugo,
     Jekyll, Docusaurus, MkDocs, GitBook, Read the Docs, VitePress) yoksa
     **yapısal geri dönüş** devreye girer: `article`/`main`/`section`/`div`,
     en son çare olarak `body`. Şablonunu tanımadığımız siteler ve kişisel
     açılış sayfaları da böylece dönüştürülür.
   - Başlık, paragraf, **tablo**, kod bloğu, alıntı, **iç içe liste**, görsel,
     görsel altı açıklaması ve ayraçlar sırayla toplanır
4. **Saf Rust PDF üretimi** (`genpdf`): seçilebilir sayfa boyutu (A4/A5/Letter/
   tablet), sayfa numaralı üstbilgi, okuma teması ve gömülü **Liberation**
   fontları → Türkçe karakterler (ğüşıöçİ) sorunsuz
5. Görseller indirilip PDF'e gömülür; yüklenemeyenler zarif notla değiştirilir
6. **Son işlem** (`lopdf`): meta veri (`/Info`, `/Lang`), başlıklardan üretilen
   **seviyeli yer imi ağacı** (içindekiler) ve tema zeminli sayfalar

> **Fontlar yalnızca kullanılan karakterlerle gömülür.** Dört sans fontu
> ~1.6 MB tutar ve tek başına PDF'in taban boyutunu belirlerdi. Metin artık
> harf harf alt kümelenir (`subsetter` + `ttf-parser`): tipik bir yazıda
> toplam font maliyeti ~140 KB'a iner.

## Çıktı kalitesi

- **Tablolar**: `<thead>`/`<th>` başlık satırı olarak, çerçeveli ızgarada
  basılır. Hücre içi listeler (`<ul>`) ve `<br>` satır sonları korunur;
  metni uzun sütunlar daha geniş yer alır. İç içe tablolar üst hücreye
  düzleştirilir.
- **Düzen (layout) tabloları**: Eski tip siteler sayfa iskeletini yüzlerce
  satırlık `<table>` ile kurar. Bunlar ızgara olarak basılmaz — içerikleri
  okuma sırasıyla akıtılır.
- **Görseller**: RGBA/alfa kanallı PNG'ler beyaz zemin üzerine bindirilir
  (genpdf alfa kanallı görselleri reddeder). Görsel, doğal boyutundan
  büyütülmez ama metin sütununu asla taşmaz; sayfa yüksekliğine de sınır
  konur. İzleyici pikseli/ikon gibi 24 px altı görseller sessizce atlanır.
  `srcset`, `data-src`, `data-lazy-src` gibi tembel yükleme kaynakları ve
  içerik-tipi belirsizken **sihirli baytlar** (magic bytes) desteklenir.
- **Listeler**: `<ol>`/`<ul>` ayrımı ve **iç içe seviyeler** korunur; maddeler
  askıda girintiyle (hanging indent) numaralanır.
- **Kod blokları**: Bloglardaki gibi **ayrı bir kutu** içinde ve **söz dizimi
  renkleriyle** basılır. Kutunun tema uyumlu dolgu zemini metnin arkasına
  çizilir; kutu **yuvarlatılmış köşeli**dir, ince bir çerçeveyle çevrelenir ve
  sol kenarında bloğun "kod" olduğunu belli eden **renk şeridi** bulunur.
  Metin kutunun **iç boşluğundan** (2 mm) sonra başlar; girintiler birebir
  korunur. Kutu genişliği her zaman metin sütununa oturur, sayfa kenarına
  dayanmaz.
  - **Renklendirme**: Yorumlar (eğik), dizeler, sayılar, anahtar sözcükler,
    tür adları ve işlev/makro çağrıları kendi rengini alır. Renkler temaya
    göre değişir (açık: GitHub paleti, koyu: GitHub Dark, sepya: kâğıt
    tonları) ve tarayıcısız çalışmak için bağımlılıksız küçük bir tarayıcıyla
    üretilir (`src/highlight.rs`) — ağır bir ayrıştırıcı yok.
  - **Dil ipucu ve rozet**: `class="language-rust"` (Prism),
    `highlight-source-python` (GitHub), `lang-py`, `data-language="go"`,
    `brush: cpp` ve saran `<div>` işaretleri okunur; `js/py/ts/sh/yml/c++` gibi
    kısa adlar da tanınır. Dil bilindiğinde kutunun sağ üstüne **dil rozeti**
    (`rust`, `python` ...) konur (`--no-code-badge` ile kapatılır).
    Tanınan diller: Rust, Python, JavaScript/TypeScript, Go, C/C++, Java,
    C#, Kotlin, Swift, Scala, Bash, SQL, Ruby, PHP, JSON, YAML/TOML, CSS,
    Markdown ve HTML/XML (etiket + öznitelik renkleriyle). Dil bulunamazsa
    yaygın kural kümesi devreye girer; kod renkli görünür ama daha genel
    kalır.
  - **Satır numaraları**: Kod iki satırdan uzunsa her satır numaralanır ve
    numaralar `│` ayracıyla koddan ayrılır (`--no-line-numbers` ile kapatılır).
    Numara sütunu metin akışının parçası olduğu için kopyalanan metinde de
    görünür ve satırlar arasında kaymaz.
  - **Uzun satırlar**: Satıra sığmayan kod satırı **görsel satırlara** bölünür;
    devam satırları numara yerine `│ »` işaretiyle girintilenir (genpdf'in
    kelime kaydırması kod girintisini bozardı). Sarım noktası ölçülerek
    bulunur: yazı tipi mono olduğu için satıra sığan karakter sayısı kesin
    olarak bilinir. Sekmeler 4 boşluk olarak hizalanır.
  - **Uzun bloklar**: Bir sayfaya sığmayan kod bloğu sayfa başına ayrı bir
    kutu olarak bölünür; hiçbir satır kaybolmaz. Bölme **akıllıdır**: sarılmış
    bir mantıksal satır asla ortadan kesilmez (satır bütün olarak devam
    sayfasına geçer), sayfanın dibinde tek başına kalan bir satır da bırakılmaz
    (son satır geri alınır). Sayfanın altına denk gelen **kısa** blok
    bölünmek yerine tümüyle sonraki sayfaya taşınır — ama yalnızca oraya
    sığıyorsa; sığmıyorsa bölünür. Devam sayfalarının üstbilgisi bunu
    belirtir: `Sayfa 7 · kod devamı`.
- **Yan içerik sızması yok**: `nav`, `aside`, `footer`, `header`, `form` ve
  `related`/`sidebar`/`widget`/`comment`/`sharedaddy`/`addthis` gibi işaretli
  alt ağaçlar içeriklerine hiç inilmeden atlanır. İşaret eşleşmesi kelime
  sınırlarına saygılıdır (`advertisement` yakalanır, `multimodal` yanlışlıkla
  yakalanmaz) ve WordPress taksonomi sınıfları (`tag-...`, `category-...`)
  arayüz kabı sayılmaz.
- **Gizli içerik**: `hidden`, `aria-hidden`, `display:none` ve
  `visibility:hidden` elemanları basılmaz.
- **Belge başlığı**: `<title>` içindeki site adı eki (`Başlık | Site`)
  temizlenir; başlığın gövdedeki tekrarı (ilk `<h1>`) düşürülür ve üstbilgi
  ilk sayfada tekrarlanmaz.
- **Yer imleri (içindekiler)**: `<h1>`…`<h6>` başlıklarından **seviyeli** bir
  `/Outlines` ağacı üretilir; her yer imi kendi başlığının sayfasına ve
  yüksekliğine konumlanır (`/XYZ` hedefi). PDF `/UseOutlines` ile açılır, yani
  tablette içindekiler paneli doğrudan gelir. Bir yazıda onlarca bölüm varsa
  ağaç iç içe görünür (Wikipedia örneğinde 55 yer imi, 4 seviye).
- **Meta veri**: `/Title`, `/Author` (varsayılan: alan adı), `/CreationDate`,
  `/ModDate`, `/Lang` (Türkçe karakterler UTF-16BE yazılır) ve `/Producer: snappdf`.
- **Tema**: `light` (varsayılan), `dark` (koyu zemin + açık metin) ve `sepia`.
  Zemin sayfanın tamamını kaplar (kenar boşlukları dâhil); metin, tablo, kod
  kutusu ve söz dizimi renkleri palete göre renklenir. Koyu temada hiçbir metin
  varsayılan siyahla basılmaz — bu, testle güvence altına alınmıştır.
- **Kod paleti** (`--code-theme`): `auto` (sayfa temasına uyar: açık→GitHub
  Light, koyu→GitHub Dark, sepya→sıcak palet), `github-light`, `github-dark`,
  `monokai`, `solarized-light`, `solarized-dark`, `sepia`. Kod paleti sayfa
  temasından **bağımsız** seçilebilir: beyaz bir sayfada Monokai ile koyu kod
  kutusu almak, dokümantasyon sitelerindeki görünümü birebir verir. Kutu
  zemini, çerçevesi, sol şeridin rengi, satır numaraları ve tüm simge renkleri
  bu paletten gelir.
- **Görsel biçimleri**: PNG ve JPEG'e ek olarak **WebP** (kayıplı VP8, kayıpsız
  VP8L ve alfa kanallı VP8X). Alfa kanallı WebP'ler kayıpsız çözülür (bunu
  `image` 0.23 yapamaz, `image-webp` yapar), beyaz zemin üzerine bindirilir ve
  PDF'e DeviceRGB olarak gömülür.

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
snappdf <url> --no-images      # görselleri PDF'e gömme (not bırakılmaz)
snappdf <url> --no-footer      # üstbilgiyi (başlık + sayfa no) kapat
snappdf <url> --no-bookmarks   # yer imi (içindekiler) üretme
snappdf --refresh-filters      # filtre listelerini yeniden indir

# Okuma deneyimi
snappdf <url> --page-size a5       # a4 | a5 | letter | tablet (162x216 mm)
snappdf <url> --theme dark         # light | dark | sepia
snappdf <url> --code-theme monokai # auto | github-light | github-dark | monokai |
                                   # solarized-light | solarized-dark | sepia
snappdf <url> --no-line-numbers    # kod bloğunda satır numarası olmasın
snappdf <url> --no-code-badge      # dil rozetini gösterme
snappdf <url> --font-size 12       # gövde yazısı, pt (8-16, varsayılan: 11)
snappdf <url> --author "Gencay"    # /Author (varsayılan: alan adı)
snappdf <url> --lang en            # /Lang etiketi (varsayılan: tr)

# Çeviri (anahtarsız, ücretsiz Google endpoint'i)
snappdf <url> --tr                 # makaleyi Türkçeye çevir (kaynak: otomatik)
snappdf <url> --tr --target-lang en  # başka dile çevir (ör. İngilizce)
```

`--tr` açıkken makale metni (başlık, paragraf, başlık, alıntı, liste, tablo
hücreleri) Google Translate'in ücretsiz `gtx` endpoint'iyle hedef dile çevrilir:

- **Kod blokları, görseller ve ayraçlara dokunulmaz** — kod çevrilmez
- Satır içi `` `kod` ``, URL ve `<etiket>` parçaları yer tutucuyla korunur;
  Google yer tutucuyu düşürürse blok **orijinaliyle** kalır (kaybolan koddan
  iyidir)
- Bir blok çevrilemezse o blok orijinal kalır; PDF üretimi asla engellenmez
- Çevrilen PDF'in `/Lang` etiketi de hedef dile ayarlanır

Tablet + kalemle çalışmak için önerilen: `--page-size tablet --theme sepia --font-size 12`
— 4:3 tablet ekranını doldurur, 12 puntoda ~55 karakter satır, kenar boşlukları
nota yer bırakır. Başlıklar yer imi ağacına işlenir, kod blokları çevrilmeden
olduğu gibi basılır; uzun yazılarda bile kaybolmazsın.

Çıktı dosyası adı host'tan türetilir: `developer.mozilla.org.pdf`,
`www.rust-lang.org.pdf` ...

Gereksinimler:
- İlk çalıştırmada filtre listeleri (~2 MB) indirilir, 7 gün önbellekte tutulur
  (macOS: `~/Library/Caches/snappdf`, Linux: `~/.cache/snappdf`,
  Windows: `%LOCALAPPDATA%\snappdf`)

## İpuçları

- **SPA/JS ağırlıklı siteler**: snappdf statik HTML'i işler; JS ile
  sonradan basılan içerik görünmez (tarayıcı olmadığı için bilinçli tercih)
- **Dosya boyutu**: fontlar yalnızca kullanılan karakterlerle gömülür (alt
  kümeleme), kod bloğu yoksa mono aile hiç gömülmez. Tipik bir yazı 150-500 KB,
  görsel yoğun sayfalar görsellerin kendi boyutuna göre büyür.
- **"makale gövdesi bulunamadı"**: sayfada anlamlı metin içeren bir kapsayıcı
  yok demektir (boş/JS kabuğu). Yapısal geri dönüş sayesinde açılış sayfaları
  ve alışılmadık şablonlar artık dönüştürülebilir.
- Yeni banner/kural gerekirse `src/extract.rs` içindeki `SKIP_TAGS` ve
  `BOILERPLATE_MARKERS` listeleri düzenlenebilir

## Mimari

```
src/
├── main.rs     CLI + orkestrasyon (engellenen görseller bloklardan da çıkarılır,
│               --tr ile makale çevirisi boru hattına girer)
├── fetch.rs    HTTP indirme (retry, içerik-tipi doğrulama, latin-1 fallback,
│               görsel biçimi sihirli baytlardan algılama)
├── lists.rs    EasyList/EasyPrivacy indirme + önbellek
├── blocker.rs  adblock-rust Engine sarmalayıcı (görsel süzme)
├── extract.rs  HTML -> makale blokları (tablo, iç içe liste, figure/figcaption)
├── images.rs   Görsel indirme/çözümleme/ölçekleme/önbellek + alfa düzleştirme
│               (PNG/JPEG/WebP; alfa kanallı WebP için image-webp)
├── fonts.rs    Font alt kümeleme: yalnızca kullanılan glifler + cmap sentezi
├── translate.rs  --tr: ücretsiz Google Translate ile makale çevirisi (kod
│               blokları ve korumalı parçalar yer tutucuyla saklanır)
├── highlight.rs  Kod için bağımlılıksız söz dizimi renklendirme (dil ipucu,
│               yorum/dize/sayı/anahtar sözcük/tür/işlev ayrımı)
├── pdf.rs      genpdf ile PDF kurulumu (sayfa boyutu, sayfa/kod paletleri,
│               tablo, liste, kod kutusu, üstbilgi) + başlık ve kod kutusu
│               konumlarını yakalayan dekoratör; kod paletleri (GitHub,
│               Monokai, Solarized) buradadır
├── postprocess.rs  lopdf ile meta veri, seviyeli yer imi ağacı, tema zemini ve
│               kod kutuları (yuvarlatılmış zemin + çerçeve + sol şerit)
└── install.rs  --install (PATH kurulumu) + --doctor (teşhis)
```

Fontlar (`assets/fonts/`, SIL OFL 1.1) binary'ye gömülüdür: harici font
dosyası gerekmez, çıktı her makinede aynı görünür. Gömmadan önce her font
kullanılan karakterlere indirilir; alt kümeleme `cmap` tablosunu kaldırdığı
(tarayıcı değil PDF için tasarlandığı) için gerekli tablo yeniden inşa edilir.

## Testler

```bash
cargo test        # 215 test
cargo clippy      # uyarısız
```

Yerel HTTP test sunucusuyla ağ yolları, içerik çıkarımı (kapsayıcı seçimi,
boilerplate budama, tablo/liste/görsel çıkarımı), PDF üretimi, görsel gömme
(alfa düzleştirme, WebP ve ölçekleme dâhil) ve kurulum akışının tamamı kapsanır.

Kayda değer testler:
- **Alt kümeleme doğruluğu**: her karakterin çizim komutları ve genişliği
  kaynak fontla birebir karşılaştırılır; yanlış bir `cmap` harfleri karıştırırdı.
- **Koyu/sepya temada siyah metin yasağı**: içerik akışı çözümlenip her metin
  gösteriminden önceki dolgu rengi denetlenir (madde imgeleri bu hatayı yakaladı).
- **Kod kutusu**: dolgu dikdörtgeninin metin sütunu genişliğinde olduğu, sayfa
  sınırlarını aşmadığı ve sayfa zemininin *üstüne* çizildiği doğrulanır; söz
  dizimi renklerinin (anahtar sözcük, işlev, dize, yorum) içerik akışına
  gerçekten yazıldığı denetlenir. Simgeleyicinin kaynak metni birebir geri
  ürettiği (kayıp/tekrar olmadığı) de test edilir.
- **Kod teması**: her paletin sayfa temasıyla birlikte PDF ürettiği, rozetin ve
  satır numaralarının kutuyu büyüttüğü, uzun satırın tam olarak bir kez
  sarıldığı ve hiçbir kod paletinde metnin varsayılan siyaha düşmediği
  doğrulanır. Kutunun **yuvarlatılmış** çizildiği (dört köşede Bézier yayı,
  yarıçap taşmayı önlemek için kutu ölçüsüne kırpılır) ve **sol renk
  şeridinin** yalnızca sol köşelerde yuvarlandığı ayrıca test edilir. `│` ayracı, `»` sarma işareti, madde imi ve ayraç karakterlerinin
  gömülü fontlarda glifi olduğu ayrıca test edilir (glifsiz karakter PDF'te boş
  kutu olarak görünürdü).
- **Sayfa bölme**: bölme hesabı birim testleriyle sabitlenmiştir — sarılmış
  satır bütün tutulur, tek satırlık yetim bırakılmaz, tek sayfadan uzun blok
  zorunlu olarak bölünür ve taşıma kararı yalnızca blok taze sayfaya sığıyorsa
  verilir (sonsuz döngü korumasıyla). Kısa bir bloğun gerçekten taşındığı ve
  devam sayfasında üstbilginin "kod devamı" notuyla büyüdüğü uçtan uca
  doğrulanır.
- **Yer imi ağacı**: seviyelerin iç içe yerleşimi, sayfa/konum hedefleri,
  `/Count` değerleri ve meta veri doğrulanır.
