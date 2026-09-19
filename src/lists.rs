use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Kullanılacak filtre listeleri.
pub const LIST_SOURCES: &[(&str, &str)] = &[
    ("easylist", "https://easylist.to/easylist/easylist.txt"),
    (
        "easyprivacy",
        "https://easylist.to/easylist/easyprivacy.txt",
    ),
];

/// Önbelleğin taze sayılacağı süre.
pub const MAX_CACHE_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// HTTP isteği için zaman aşımı.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// snappdf önbellek klasörünü döndürür (yoksa oluşturur).
pub fn cache_dir() -> Result<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Caches"))
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let dir = base.join("snappdf");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Önbellekteki bir liste dosyasının taze olup olmadığı.
pub fn is_fresh(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|age| age < MAX_CACHE_AGE)
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Tüm filtre önbelleğini siler (--refresh-filters için).
#[allow(dead_code)]
pub fn clear_cache() -> Result<(usize, u64)> {
    clear_cache_in(&cache_dir()?)
}

/// Verilen klasördeki liste dosyalarını siler; (adet, bayt) döner.
#[allow(dead_code)]
pub fn clear_cache_in(dir: &Path) -> Result<(usize, u64)> {
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for (name, _) in LIST_SOURCES {
        let path = dir.join(format!("{name}.txt"));
        if let Ok(meta) = std::fs::metadata(&path) {
            bytes += meta.len();
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok((removed, bytes))
}

/// Tek bir listeyi dener: önbellekten yükler ya da indirir.
/// Dönen değer: (liste metni, önbellekten mi geldiği).
async fn load_one(
    name: &str,
    url: &str,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<(String, bool)> {
    let path = dir.join(format!("{name}.txt"));
    if is_fresh(&path) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            eprintln!("[filtre] {name} önbellekten yüklendi");
            return Ok((text, true));
        }
    }

    eprintln!("[filtre] {name} indiriliyor...");
    let resp = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("{name} indirilemedi: {url}"))?
        .error_for_status()
        .with_context(|| format!("{name} için sunucu hatası: {url}"))?;

    let text = resp.text().await.context("Liste gövdesi okunamadı")?;
    let _ = std::fs::write(&path, &text);
    Ok((text, false))
}

/// Tüm listeleri birleştirilmiş tek bir metin olarak döndürür.
/// Önce diskteki güncel önbelleğe bakar, gerekirse indirir.
/// `refresh = true` ise önbelleğe hiç bakılmaz, her liste yeniden indirilir.
pub async fn load_combined_with(refresh: bool, dir_override: Option<&Path>) -> Result<String> {
    let dir = match dir_override {
        Some(d) => {
            std::fs::create_dir_all(d)?;
            d.to_path_buf()
        }
        None => cache_dir()?,
    };
    let client = reqwest::Client::builder()
        .user_agent("snappdf/0.1 (+filter-list-fetcher)")
        .build()
        .context("HTTP istemcisi oluşturulamadı")?;

    let mut combined = String::new();
    for (name, url) in LIST_SOURCES {
        let (text, _) = if refresh {
            eprintln!("[filtre] {name} yenileniyor (--refresh-filters)...");
            (fetch_one(name, url, dir.as_path(), &client).await?, true)
        } else {
            load_one(name, url, &dir, &client).await?
        };
        combined.push_str(&text);
        combined.push('\n');
    }

    Ok(combined)
}

/// Birleştirilmiş kural metnini adblock Engine'e çevirir (CLI'nin kullandığı yol).
#[allow(dead_code)]
pub fn build_engine(combined_rules: String) -> Result<crate::blocker::Blocker> {
    crate::blocker::Blocker::new(combined_rules)
}

/// Sadece ağdan indirir (önbelleğe bakmadan), diske yazar.
async fn fetch_one(name: &str, url: &str, dir: &Path, client: &reqwest::Client) -> Result<String> {
    let resp = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("{name} indirilemedi: {url}"))?
        .error_for_status()
        .with_context(|| format!("{name} için sunucu hatası: {url}"))?;
    let text = resp.text().await.context("Liste gövdesi okunamadı")?;
    let _ = std::fs::write(dir.join(format!("{name}.txt")), &text);
    Ok(text)
}

/// CLI'nin kullandığı üst düzey yükleyici (geriye dönük uyum için).
#[allow(dead_code)]
pub async fn load_combined() -> Result<String> {
    load_combined_with(false, None).await
}

/// Önbellek yaşı (teşhis için): en yeni dosyanın yaşı.
pub fn cache_age(dir: Option<&Path>) -> Option<Duration> {
    let dir = dir.map(|p| p.to_path_buf()).or_else(|| cache_dir().ok())?;
    LIST_SOURCES
        .iter()
        .filter_map(|(name, _)| {
            let meta = std::fs::metadata(dir.join(format!("{name}.txt"))).ok()?;
            meta.modified().ok()?.elapsed().ok()
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "snappdf-test-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cache_dir_creates_and_returns_existing() {
        let d = cache_dir().unwrap();
        assert!(d.ends_with("snappdf"));
        assert!(d.is_dir());
        // İkinci çağrı da başarılı olmalı (idempotent).
        assert_eq!(cache_dir().unwrap(), d);
    }

    #[test]
    fn is_fresh_fresh_file() {
        let td = TempDir::new("fresh");
        let p = td.path().join("a.txt");
        std::fs::write(&p, "test").unwrap();
        assert!(is_fresh(&p));
    }

    #[test]
    fn is_fresh_missing_file() {
        let td = TempDir::new("missing");
        assert!(!is_fresh(&td.path().join("yok.txt")));
    }

    #[test]
    fn is_fresh_stale_file() {
        let td = TempDir::new("stale");
        let p = td.path().join("eski.txt");
        std::fs::write(&p, "test").unwrap();

        // Dosyanın modified zamanını MAX_CACHE_AGE'den eskiye çek.
        let old = SystemTime::now() - MAX_CACHE_AGE - Duration::from_secs(60);
        let file = std::fs::File::options().write(true).open(&p).unwrap();
        file.set_modified(old).unwrap();
        drop(file);

        assert!(!is_fresh(&p));
    }

    #[test]
    fn clear_cache_removes_only_list_files() {
        let td = TempDir::new("clear");
        let mut expected_bytes = 0u64;
        for (name, _) in LIST_SOURCES {
            let p = td.path().join(format!("{name}.txt"));
            let content = format!("icerik-{name}");
            expected_bytes += content.len() as u64;
            std::fs::write(&p, content).unwrap();
        }
        // Listeye ait olmayan dosya dokunulmamalı.
        std::fs::write(td.path().join("diger.txt"), "dokunma").unwrap();

        let (removed, bytes) = clear_cache_in(td.path()).unwrap();
        assert_eq!(removed, LIST_SOURCES.len());
        assert_eq!(bytes, expected_bytes);
        assert!(!td.path().join("easylist.txt").exists());
        assert!(!td.path().join("easyprivacy.txt").exists());
        assert!(td.path().join("diger.txt").exists());
    }

    #[test]
    fn clear_cache_empty_dir_is_zero() {
        let td = TempDir::new("clearempty");
        let (removed, bytes) = clear_cache_in(td.path()).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn cache_age_none_when_empty() {
        let td = TempDir::new("agenone");
        assert!(cache_age(Some(td.path())).is_none());
    }

    #[test]
    fn cache_age_some_when_present() {
        let td = TempDir::new("agesome");
        for (name, _) in LIST_SOURCES {
            std::fs::write(td.path().join(format!("{name}.txt")), "x").unwrap();
        }
        let age = cache_age(Some(td.path())).unwrap();
        assert!(age < Duration::from_secs(5));
    }

    #[test]
    fn list_sources_are_valid_urls() {
        assert!(LIST_SOURCES.len() >= 2);
        for (name, url) in LIST_SOURCES {
            assert!(!name.is_empty());
            assert!(url.starts_with("https://"));
        }
    }

    #[test]
    fn max_cache_age_is_seven_days() {
        assert_eq!(MAX_CACHE_AGE, Duration::from_secs(7 * 24 * 3600));
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "snappdf-loader-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn load_combined_uses_fresh_cache_without_network() {
        let dir = temp_dir("cachehit");
        std::fs::write(dir.join("easylist.txt"), "||cached.example^").unwrap();
        std::fs::write(dir.join("easyprivacy.txt"), "||izleyici.example^").unwrap();

        let combined = load_combined_with(false, Some(&dir)).await.unwrap();
        assert!(combined.contains("||cached.example^"));
        assert!(combined.contains("||izleyici.example^"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn load_combined_with_empty_cache_dir_fails_gracefully() {
        // Boş önbellek: ağdan indirme denenir; test ortamı engelliyse bile
        // hata net olmalı (panik değil).
        let dir = temp_dir("cacheempty");
        let result = load_combined_with(false, Some(&dir)).await;
        // Sonuç ne olursa olsun panik yok: Ok (ağ açıksa) ya da anlamlı Err.
        if let Err(e) = result {
            assert!(!e.to_string().is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_engine_wires_blocker() {
        let blocker = build_engine("||ad.example^\n".to_string()).unwrap();
        assert!(blocker.should_block("https://ad.example/x.js", "https://site.com", "script"));
    }
}
