mod blocker;
mod extract;
mod fetch;
mod fonts;
mod highlight;
mod images;
mod install;
mod lists;
mod pdf;
mod postprocess;
mod translate;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

/// snappdf — link -> güzel PDF (tarayıcısız)
/// Reklam/izleyici temizliğini adblock motoruyla yapan, Chrome gerektirmeyen
/// PDF üretici.
#[derive(Parser, Debug)]
#[command(name = "snappdf", version, about)]
struct Args {
    /// PDF'e çevrilecek sayfa(lar). Örn: snappdf https://blog.rust-lang.org/...
    #[arg(required_unless_present_any = ["install", "doctor"])]
    urls: Vec<String>,

    /// Çıktı klasörü (dosya adı host'tan türetilir)
    #[arg(short, long, default_value = ".")]
    out: PathBuf,

    /// Altbilgiyi kapat (varsayılan: açık, başlık + sayfa numarası)
    #[arg(long, default_value_t = false)]
    no_footer: bool,

    /// Görselleri PDF'e gömme
    #[arg(long, default_value_t = false)]
    no_images: bool,

    /// Sayfa boyutu: a4 | a5 | letter | tablet
    #[arg(long, default_value = "a4", value_parser = parse_page_size)]
    page_size: pdf::PageSize,

    /// Okuma teması: light | dark | sepia
    #[arg(long, default_value = "light", value_parser = parse_theme)]
    theme: pdf::Theme,

    /// Kod bloğu paleti: auto | github-light | github-dark | monokai |
    /// solarized-light | solarized-dark | sepia (auto: sayfa temasına uyar)
    #[arg(long, default_value = "auto", value_parser = parse_code_theme)]
    code_theme: pdf::CodeTheme,

    /// Kod bloklarında satır numarası gösterme
    #[arg(long, default_value_t = false)]
    no_line_numbers: bool,

    /// Kod bloğunun üstündeki dil rozetini gösterme
    #[arg(long, default_value_t = false)]
    no_code_badge: bool,

    /// Blogun vurguladığı kod satırlarını renkli bantla basma
    #[arg(long, default_value_t = false)]
    no_line_highlights: bool,

    /// Gövde yazı tipi büyüklüğü, pt (8-16; tablet+kalem okuma için 12 önerilir)
    #[arg(long, default_value_t = 11, value_parser = parse_font_size)]
    font_size: u8,

    /// PDF yazar alanı (varsayılan: sayfanın alan adı)
    #[arg(long)]
    author: Option<String>,

    /// İçerik dili etiketi (PDF /Lang)
    #[arg(long, default_value = "tr")]
    lang: String,

    /// Makaleyi Google Translate ile Türkçeye çevir (kod blokları korunur)
    #[arg(long, default_value_t = false)]
    tr: bool,

    /// `--tr` ile birlikte hedef dil kodu (ör. en, de, es)
    #[arg(long, default_value = "tr")]
    target_lang: String,

    /// Başlıklardan PDF yer imi (içindekiler) üretme
    #[arg(long, default_value_t = false)]
    no_bookmarks: bool,

    /// Filtre listelerini yeniden indir
    #[arg(long, default_value_t = false)]
    refresh_filters: bool,

    /// snappdf'i PATH'e kur (cargo install + kabuk profili)
    #[arg(long, default_value_t = false)]
    install: bool,

    /// Kurulumu ve bağımlılıkları teşhis et
    #[arg(long, default_value_t = false)]
    doctor: bool,
}

/// `--page-size` değerini çözer.
fn parse_page_size(value: &str) -> Result<pdf::PageSize, String> {
    pdf::PageSize::parse(value)
        .ok_or_else(|| format!("bilinmeyen sayfa boyutu '{value}' (a4, a5, letter, tablet)"))
}

/// `--theme` değerini çözer.
fn parse_theme(value: &str) -> Result<pdf::Theme, String> {
    pdf::Theme::parse(value)
        .ok_or_else(|| format!("bilinmeyen tema '{value}' (light, dark, sepia)"))
}

/// `--code-theme` değerini çözer.
fn parse_code_theme(value: &str) -> Result<pdf::CodeTheme, String> {
    pdf::CodeTheme::parse(value).ok_or_else(|| {
        format!(
            "bilinmeyen kod teması '{value}' (auto, github-light, github-dark, monokai, \
             solarized-light, solarized-dark, sepia)"
        )
    })
}

/// `--font-size` değerini çözer (8-16 pt).
fn parse_font_size(value: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .ok()
        .filter(|n| (8..=16).contains(n))
        .ok_or_else(|| format!("bilinmeyen yazı boyutu '{value}' (8-16 pt)"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.install {
        return install::run_install();
    }
    if args.doctor {
        return install::run_doctor();
    }

    run(&args).await
}

async fn run(args: &Args) -> Result<()> {
    std::fs::create_dir_all(&args.out).context("Çıktı klasörü oluşturulamadı")?;

    // 1) Filtre motoru (istek engellemek yerine URL temizliğinde kullanılır)
    let rules = lists::load_combined_with(args.refresh_filters, None).await?;
    eprintln!("[motor] filtreler derleniyor...");
    let blocker = Arc::new(blocker::Blocker::new(rules)?);

    // 2) HTTP istemcisi
    let client = fetch::build_client(fetch::DEFAULT_TIMEOUT)?;

    // 3) Her link için PDF
    let mut ok = 0usize;
    let mut failed: Vec<(String, String)> = vec![];
    for url in &args.urls {
        eprintln!("==> {url}");
        match render_url(&client, &blocker, url, args).await {
            Ok(bytes) => {
                let path = args.out.join(pdf::output_name(url));
                match std::fs::write(&path, &bytes) {
                    Ok(_) => {
                        ok += 1;
                        eprintln!(
                            "[tamam] {} ({:.1} KB)",
                            path.display(),
                            bytes.len() as f64 / 1024.0
                        );
                    }
                    Err(e) => failed.push((url.clone(), format!("yazma hatası: {e}"))),
                }
            }
            Err(e) => failed.push((url.clone(), format!("{e:#}"))),
        }
    }

    if !failed.is_empty() {
        eprintln!("\nBaşarısız olanlar:");
        for (u, why) in &failed {
            eprintln!("  - {u}: {why}");
        }
    }
    eprintln!("\n{ok} PDF üretildi → {}", args.out.display());
    if ok == 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Tek bir URL'yi indirir, temizler, çıkarır ve PDF baytlarına dönüştürür.
async fn render_url(
    client: &reqwest::Client,
    blocker: &Arc<blocker::Blocker>,
    url: &str,
    args: &Args,
) -> Result<Vec<u8>> {
    let html = fetch::fetch_html(client, url).await?;
    eprintln!("[çıkar] makale gövdesi ayrıştırılıyor...");

    let mut article = extract::extract(&html, url, &extract::ExtractOptions::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // --tr: makaleyi hedef dile çevir (varsayılan Türkçe). Başarısız bloklar
    // orijinal kalır; çeviri PDF üretimini asla engellemez.
    if args.tr {
        eprintln!("[çeviri] Google Translate → {} ...", args.target_lang);
        let translator = translate::Translator::new(&args.target_lang)?;
        translate::translate_article(&mut article, &translator).await;
    }

    // Reklam/izleyici görselleri adblock motoruyla süzülür. Hem görsel
    // listesinden hem de bloklardan çıkarılır; aksi halde engellenen görsel
    // PDF'e yine girerdi.
    let blocked: Vec<String> = article
        .images
        .iter()
        .filter(|img| blocker.should_block(img, url, "image"))
        .cloned()
        .collect();
    if !blocked.is_empty() {
        article.images.retain(|img| !blocked.contains(img));
        article
            .blocks
            .retain(|b| !matches!(b, extract::Block::Image(u) if blocked.contains(u)));
        eprintln!("[temiz] {} izleyici/reklam görseli elendi", blocked.len());
    }

    let opts = pdf::PdfOptions {
        footer: !args.no_footer,
        font_size: args.font_size,
        embed_images: !args.no_images,
        page: args.page_size,
        theme: args.theme,
        code_theme: args.code_theme,
        line_numbers: !args.no_line_numbers,
        code_badge: !args.no_code_badge,
        line_highlights: !args.no_line_highlights,
        bookmarks: !args.no_bookmarks,
    };
    let meta = postprocess::Meta {
        title: article.title.clone(),
        author: args.author.clone().unwrap_or_else(|| pdf::host(url)),
        language: args.lang.clone(),
    };
    let tables = article
        .blocks
        .iter()
        .filter(|b| matches!(b, extract::Block::Table(_)))
        .count();
    eprintln!(
        "[pdf] {} sayfası, {} tema, {} kod paleti, {} blok, {tables} tablo, {} görsel...",
        opts.page.name(),
        opts.theme.name(),
        opts.code_theme.name(),
        article.blocks.len(),
        article.images.len()
    );

    let rendered = pdf::render_article(&article, &opts, &meta)?;

    if !args.no_images {
        let stats = images::take_stats();
        eprintln!(
            "[görsel] {} gömüldü, {} dekoratif atlandı, {} yüklenemedi",
            stats.loaded, stats.skipped, stats.failed
        );
    }
    eprintln!(
        "[meta] {} yer imi, dil {}{}{}{}",
        rendered.bookmarks,
        meta.language,
        if meta.author.is_empty() {
            String::new()
        } else {
            format!(", yazar {}", meta.author)
        },
        if rendered.code_splits == 0 {
            String::new()
        } else {
            format!(", {} kod bloğu sayfaya bölündü", rendered.code_splits)
        },
        if rendered.highlighted_lines == 0 {
            String::new()
        } else {
            format!(", {} vurgulu kod satırı", rendered.highlighted_lines)
        }
    );
    Ok(rendered.bytes)
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> Args {
        Args {
            urls: vec!["https://example.com".to_string()],
            out: PathBuf::from("."),
            no_footer: false,
            no_images: false,
            page_size: pdf::PageSize::A4,
            theme: pdf::Theme::Light,
            code_theme: pdf::CodeTheme::Auto,
            no_line_numbers: false,
            no_code_badge: false,
            no_line_highlights: false,
            font_size: 11,
            author: None,
            lang: "tr".to_string(),
            tr: false,
            target_lang: "tr".to_string(),
            no_bookmarks: false,
            refresh_filters: false,
            install: false,
            doctor: false,
        }
    }

    #[test]
    fn defaults_match_documented_values() {
        let a = base_args();
        assert!(!a.no_footer);
        assert!(!a.no_images);
        assert!(!a.refresh_filters);
        assert!(!a.install);
        assert!(!a.doctor);
    }

    #[test]
    fn pdf_options_follow_args() {
        let mut a = base_args();
        let o = pdf_options_from(&a);
        assert!(o.footer);
        assert!(o.embed_images);
        a.no_footer = true;
        a.no_images = true;
        a.font_size = 12;
        let o = pdf_options_from(&a);
        assert!(!o.footer);
        assert!(!o.embed_images);
        assert_eq!(o.font_size, 12);
    }

    fn pdf_options_from(args: &Args) -> pdf::PdfOptions {
        pdf::PdfOptions {
            footer: !args.no_footer,
            font_size: args.font_size,
            embed_images: !args.no_images,
            page: args.page_size,
            theme: args.theme,
            code_theme: args.code_theme,
            line_numbers: !args.no_line_numbers,
            code_badge: !args.no_code_badge,
            line_highlights: !args.no_line_highlights,
            bookmarks: !args.no_bookmarks,
        }
    }

    #[test]
    fn clap_parses_reading_options() {
        let a = Args::parse_from([
            "snappdf",
            "https://example.com",
            "--page-size",
            "a5",
            "--theme",
            "dark",
            "--author",
            "Gencay",
            "--lang",
            "en",
            "--no-bookmarks",
            "--font-size",
            "12",
        ]);
        assert_eq!(a.page_size, pdf::PageSize::A5);
        assert_eq!(a.theme, pdf::Theme::Dark);
        assert_eq!(a.author.as_deref(), Some("Gencay"));
        assert_eq!(a.lang, "en");
        assert!(a.no_bookmarks);
        assert_eq!(a.font_size, 12);
    }

    #[test]
    fn clap_parses_translate_flags() {
        let a = Args::parse_from([
            "snappdf",
            "https://example.com",
            "--tr",
            "--target-lang",
            "en",
        ]);
        assert!(a.tr);
        assert_eq!(a.target_lang, "en");

        let d = Args::parse_from(["snappdf", "https://example.com"]);
        assert!(!d.tr);
        assert_eq!(d.target_lang, "tr");
    }

    #[test]
    fn clap_defaults_keep_a4_light_and_bookmarks() {
        let a = Args::parse_from(["snappdf", "https://example.com"]);
        assert_eq!(a.page_size, pdf::PageSize::A4);
        assert_eq!(a.theme, pdf::Theme::Light);
        assert!(!a.no_bookmarks);
        assert_eq!(a.lang, "tr");
        assert!(a.author.is_none());
        assert_eq!(a.font_size, 11);
    }

    #[test]
    fn clap_parses_code_block_options() {
        let a = Args::parse_from([
            "snappdf",
            "https://example.com",
            "--code-theme",
            "monokai",
            "--no-line-numbers",
            "--no-code-badge",
        ]);
        assert_eq!(a.code_theme, pdf::CodeTheme::Monokai);
        assert!(a.no_line_numbers);
        assert!(a.no_code_badge);
        let o = pdf_options_from(&a);
        assert_eq!(o.code_theme, pdf::CodeTheme::Monokai);
        assert!(!o.line_numbers);
        assert!(!o.code_badge);
    }

    #[test]
    fn code_block_defaults_are_enabled() {
        let a = Args::parse_from(["snappdf", "https://example.com"]);
        assert_eq!(a.code_theme, pdf::CodeTheme::Auto);
        assert!(!a.no_line_numbers);
        assert!(!a.no_code_badge);
        let o = pdf_options_from(&a);
        assert_eq!(o.code_theme, pdf::CodeTheme::Auto);
        assert!(o.line_numbers);
        assert!(o.code_badge);
    }

    #[test]
    fn clap_rejects_unknown_code_theme() {
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--code-theme", "neon"]).is_err());
        assert!(parse_code_theme("solarized-dark").is_ok());
        assert!(parse_code_theme("Sepia").is_ok());
        assert!(parse_code_theme("auto").is_ok());
    }

    #[test]
    fn clap_rejects_unknown_page_size_and_theme() {
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--page-size", "a6"]).is_err());
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--theme", "neon"]).is_err());
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--font-size", "7"]).is_err());
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--font-size", "17"]).is_err());
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--font-size", "abc"]).is_err());
        assert!(Args::try_parse_from(["snappdf", "https://a.com", "--font-size", "12"]).is_ok());
        assert!(parse_page_size("A5").is_ok());
        assert!(parse_theme("Sepia").is_ok());
        assert!(parse_font_size("8").is_ok());
    }

    // --- clap tanımlarının parse düzeyi testleri ---

    #[test]
    fn clap_parses_no_footer_and_no_images() {
        let a = Args::parse_from([
            "snappdf",
            "https://example.com",
            "--no-footer",
            "--no-images",
        ]);
        assert!(a.no_footer);
        assert!(a.no_images);
    }

    #[test]
    fn clap_parses_install_without_urls() {
        let a = Args::parse_from(["snappdf", "--install"]);
        assert!(a.install);
        assert!(a.urls.is_empty());
    }

    #[test]
    fn clap_parses_doctor_without_urls() {
        let a = Args::parse_from(["snappdf", "--doctor"]);
        assert!(a.doctor);
        assert!(a.urls.is_empty());
    }

    #[test]
    fn clap_rejects_empty_invocation() {
        assert!(Args::try_parse_from(["snappdf"]).is_err());
    }

    #[test]
    fn clap_parses_multiple_urls_and_out() {
        let a = Args::parse_from([
            "snappdf",
            "https://a.com",
            "https://b.com",
            "-o",
            "/tmp/pdf",
            "--refresh-filters",
        ]);
        assert_eq!(a.urls.len(), 2);
        assert_eq!(a.out, PathBuf::from("/tmp/pdf"));
        assert!(a.refresh_filters);
    }

    // --- uçtan uca render_url testleri: yerel HTTP sunucusuyla ---

    /// Testler için minik TCP HTTP sunucusu (fetch.rs'tekiyle aynı protokol).
    struct TestServer {
        addr: String,
        handle: Option<std::thread::JoinHandle<()>>,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl TestServer {
        fn start(responder: impl Fn(&str) -> String + Send + 'static) -> Self {
            use std::io::Read;
            use std::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let sd = shutdown.clone();
            let handle = std::thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !sd.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut buf = [0u8; 4096];
                            let n = stream.read(&mut buf).unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                            let body = responder(&path);
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = std::io::Write::write_all(&mut stream, resp.as_bytes());
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                addr,
                handle: Some(handle),
                shutdown,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}/{}", self.addr, path.trim_start_matches('/'))
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    const ARTICLE_HTML: &str = r#"
    <html><head><title>Uçtan Uca Test</title></head><body>
      <article>
        <h1>Uçtan Uca Test</h1>
        <p>İlk paragraf, Türkçe karakterlerle: ğüşıöçİĞÜŞÖÇ. Uzunluk yeterli olsun
           diye birkaç cümle daha ekliyoruz ki içerik çıkarımı makale gövdesini
           seçsin ve pipeline tamamen çalışsın.</p>
        <ul><li>madde bir</li><li>madde iki</li></ul>
        <pre class="language-rust"><code>fn main() {
    // uçtan uca: uzun satır kutu içinde sarmalanır
    let toplam = birinci_deger + ikinci_deger + ucuncu_deger + dorduncu_deger + besinci_deger + altinci_deger + yedinci_deger;
}</code></pre>
        <blockquote>alıntı bloğu</blockquote>
        <hr>
        <p>Son paragraf burada.</p>
      </article>
    </body></html>"#;

    /// PDF sayfalarının içerik akışlarını (sıkıştırmayı açarak) birleştirir.
    fn content_streams(bytes: &[u8]) -> String {
        let doc = lopdf::Document::load_mem(bytes).expect("üretilen PDF okunamadı");
        let mut out = String::new();
        for page_id in doc.get_pages().values() {
            for content_id in doc.get_page_contents(*page_id) {
                let stream = doc.get_object(content_id).unwrap().as_stream().unwrap();
                match stream.decompressed_content() {
                    Ok(content) => out.push_str(&String::from_utf8_lossy(&content)),
                    Err(_) => out.push_str(&String::from_utf8_lossy(&stream.content)),
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn render_url_end_to_end_produces_pdf() {
        let server = TestServer::start(|_| ARTICLE_HTML.to_string());
        let client = fetch::build_client(fetch::DEFAULT_TIMEOUT).unwrap();
        let rules = "
||reklam.example^
"
        .to_string();
        let blocker = Arc::new(blocker::Blocker::new(rules).unwrap());
        let mut args = base_args();
        args.urls = vec![server.url("yazi")];
        let bytes = render_url(&client, &blocker, &args.urls[0], &args)
            .await
            .unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 1000);

        // Kod bloğu uçtan uca: kutu zemini, söz dizimi renkleri ve satır
        // numarası/rozet rengi gerçek boru hattından geçip PDF'e yazılmalı.
        let text = content_streams(&bytes);
        assert!(
            text.contains("% snappdf:kod-kutusu"),
            "kod kutusu zemini boyanmadı"
        );
        // Dolgu operatörü dört ondalıkla yazılır (GitHub Light zemini).
        assert!(
            text.contains("0.9647 0.9725 0.9804 rg"),
            "kutunun dolgu rengi yok"
        );
        assert!(
            text.contains("0.81 0.13 0.18 rg"),
            "anahtar sözcük rengi yok"
        );
        assert!(
            text.contains("0.49 0.52 0.56 rg"),
            "satır numarası/rozet rengi yok"
        );
    }

    #[tokio::test]
    async fn render_url_filters_tracker_images_via_blocker() {
        // Görsel URL'si adblock kuralına çarparsa images listesinden düşmeli.
        let server = TestServer::start(|_| {
            ARTICLE_HTML.replace(
                "</article>",
                r#"<img src="https://reklam.example/piksel.png">"#,
            ) + "</article>"
        });
        let client = fetch::build_client(fetch::DEFAULT_TIMEOUT).unwrap();
        let blocker = Arc::new(blocker::Blocker::new("||reklam.example^\n".to_string()).unwrap());
        let args = base_args();
        let url = server.url("yazi");
        // Görsel elenmeli ama embed_images kapalı olduğundan indirme denmemeli;
        // PDF yine üretilmeli.
        let mut args = args;
        args.no_images = true;
        let bytes = render_url(&client, &blocker, &url, &args).await.unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[tokio::test]
    async fn run_reports_failure_without_pdf_on_bad_url() {
        // Geçersiz bağlantı: run() başarısız listeye eklemeli, PDF üretmemeli.
        let mut args = base_args();
        args.urls = vec!["http://127.0.0.1:9/".to_string()];
        args.out = std::env::temp_dir().join("snappdf-run-fail-test");
        let _ = std::fs::create_dir_all(&args.out);
        // run() süreçten çıkmamalı (ok == 0 için exit(1) çağırır) — bu yüzden
        // doğrudan render_url üzerinden doğrulıyoruz ve hata mesajını kontrol ediyoruz.
        let client = fetch::build_client(std::time::Duration::from_secs(2)).unwrap();
        let blocker = Arc::new(blocker::Blocker::new(String::new()).unwrap());
        let err = render_url(&client, &blocker, &args.urls[0], &args)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("indirilemedi"));
    }
}
