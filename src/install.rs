use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// snappdf'in kurulacağı klasör (cargo install hedefi).
pub fn cargo_bin_dir() -> Result<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .map(|c| c.join("bin"))
        .context("CARGO_HOME ya da HOME bulunamadı")
}

/// Kabuk yapılandırma dosyası adı.
pub fn shell_rc_name() -> &'static str {
    match std::env::var("SHELL").ok().as_deref() {
        Some(s) if s.ends_with("zsh") => ".zshrc",
        Some(s) if s.ends_with("bash") => ".bashrc",
        _ => ".profile",
    }
}

/// PATH girdisini dosya içeriğine ekler; zaten varsa dokunmaz.
/// Dönen değer: (değiştirildi mi, eklenen satır).
pub fn append_path_line(rc_path: &Path, bin_dir: &Path) -> Result<(bool, String)> {
    let line = format!("export PATH=\"{}:$PATH\"  # snappdf", bin_dir.display());
    let existing = std::fs::read_to_string(rc_path).unwrap_or_default();
    if existing.contains("# snappdf") || existing.contains(&bin_dir.display().to_string()) {
        return Ok((false, line));
    }
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&line);
    new_content.push('\n');
    std::fs::write(rc_path, &new_content)
        .with_context(|| format!("{} yazılamadı", rc_path.display()))?;
    Ok((true, line))
}

/// `snappdf` şu anda PATH'te mi?
pub fn is_on_path(bin_dir: &Path) -> bool {
    match std::env::var_os("PATH") {
        Some(p) => std::env::split_paths(&p).any(|d| d == bin_dir && bin_dir.exists()),
        None => false,
    }
}

/// `snappdf` ikili dosyası kurulu mu?
pub fn binary_installed(bin_dir: &Path) -> bool {
    bin_dir.join("snappdf").exists()
}

/// Kurulumdaki bütün adımları sırayla çalıştırır.
///
/// `dry_run = true` iken cargo install çalıştırılmaz; yalnızca adım planı
/// döner (testlerde ve --install önizlemesi için kullanılır).
#[allow(dead_code)]
pub fn perform_install_with(dry_run: bool) -> Result<Vec<String>> {
    if dry_run {
        let bin_dir = cargo_bin_dir()?;
        let rc = home_dir()?.join(shell_rc_name());
        return Ok(vec![
            "cargo install --path . — (önizleme: atlandı)".to_string(),
            format!("{} PATH denetlenecek", bin_dir.display()),
            format!("{} güncellenecek", rc.display()),
        ]);
    }
    perform_install()
}

/// Kurulumdaki bütün adımları sırayla çalıştırır.
pub fn perform_install() -> Result<Vec<String>> {
    let mut steps = Vec::new();

    // 1) cargo install --path .
    let status = Command::new("cargo")
        .args(["install", "--path", "."])
        .status()
        .context("cargo bulunamadı — Rust kurun: https://rustup.rs")?;
    if !status.success() {
        bail!("cargo install başarısız oldu");
    }
    steps.push("cargo install --path . — tamam".to_string());

    // 2) PATH kontrolü ve gerekiyorsa kabuk profiline ekleme
    let bin_dir = cargo_bin_dir()?;
    if is_on_path(&bin_dir) {
        steps.push(format!("{} zaten PATH'te", bin_dir.display()));
    } else {
        let rc = home_dir()?.join(shell_rc_name());
        let (changed, line) = append_path_line(&rc, &bin_dir)?;
        if changed {
            steps.push(format!("{} içine eklendi: {line}", rc.display()));
            steps.push(
                "Yeni terminalde otomatik görünür; şimdi için: `source ~/<rc-dosyan>`".to_string(),
            );
        } else {
            steps.push(format!("PATH satırı {} içinde zaten var", rc.display()));
        }
        steps.push(format!(
            "Şu anki oturumda denemek için: export PATH=\"{}:$PATH\"",
            bin_dir.display()
        ));
    }

    Ok(steps)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME bulunamadı")
}

/// --install: kurulumu çalıştır ve sonucu yazdır.
pub fn run_install() -> Result<()> {
    println!("snappdf kuruluyor...\n");
    let steps = perform_install()?;
    for s in &steps {
        println!("  ✓ {s}");
    }
    println!("\nKurulum tamamlandı. Test: snappdf --doctor");
    Ok(())
}

/// --doctor: teşhis raporu.
pub fn run_doctor() -> Result<()> {
    println!("snappdf teşhis\n");
    let mut problems = 0usize;

    // 1) İkili dosya
    let bin_dir = cargo_bin_dir()?;
    if binary_installed(&bin_dir) {
        println!("  ✓ ikili: {}", bin_dir.join("snappdf").display());
    } else {
        problems += 1;
        println!(
            "  ✗ ikili bulunamadı: {}",
            bin_dir.join("snappdf").display()
        );
        println!("    Çözüm: cargo install --path .");
    }

    // 2) PATH
    if is_on_path(&bin_dir) {
        println!("  ✓ {} PATH'te", bin_dir.display());
    } else {
        problems += 1;
        println!(
            "  ✗ {} PATH'te değil → 'komut bulunamıyor' hatasının sebebi",
            bin_dir.display()
        );
        println!("    Çözüm: snappdf --install  (kabuk profiline otomatik ekler)");
    }

    // 3) Ağ erişimi (filtre kaynağına kabaca bakılır; indirme denmez)
    println!("  ℹ filtre kaynakları: EasyList + EasyPrivacy (ilk çalıştırmada indirilir)");

    // 4) Filtre önbelleği
    match crate::lists::cache_dir() {
        Ok(d) => {
            let age = crate::lists::cache_age(Some(&d));
            match age {
                Some(a) => println!(
                    "  ✓ filtre önbelleği: {} (yaş: {} sn)",
                    d.display(),
                    a.as_secs()
                ),
                None => println!(
                    "  ✓ filtre önbelleği: {} (boş — ilk çalıştırmada indirilir)",
                    d.display()
                ),
            }
        }
        Err(e) => {
            println!("  ✗ önbellek klasörü: {e:#}");
            problems += 1;
        }
    }

    if problems == 0 {
        println!("\nHer şey yolunda. Kullanım: snappdf <url>");
        Ok(())
    } else {
        bail!("{problems} sorun bulundu (yukarıda)")
    }
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "snappdf-inst-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn cargo_bin_dir_from_cargo_home() {
        // CARGO_HOME kuruluysa bin alt klasörünü döndürmeli.
        if let Some(ch) = std::env::var_os("CARGO_HOME") {
            let d = cargo_bin_dir().unwrap();
            assert_eq!(d, PathBuf::from(ch).join("bin"));
        }
    }

    #[test]
    fn cargo_bin_dir_falls_back_to_home() {
        // HOME varken bu fonksiyon hata döndürmemeli.
        if std::env::var_os("HOME").is_some() || std::env::var_os("CARGO_HOME").is_some() {
            let _ = cargo_bin_dir().unwrap();
        }
    }

    #[test]
    fn shell_rc_name_is_known_suffix() {
        let n = shell_rc_name();
        assert!([".zshrc", ".bashrc", ".profile"].contains(&n));
    }

    #[test]
    fn append_path_line_adds_when_missing() {
        let home = temp_home("add");
        let rc = home.join(".zshrc");
        let bin = home.join("cargo-bin");
        let (changed, line) = append_path_line(&rc, &bin).unwrap();
        assert!(changed);
        assert!(line.contains(bin.to_str().unwrap()));
        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(content.contains(&line));
        assert!(content.contains("# snappdf"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn append_path_line_is_idempotent() {
        let home = temp_home("idem");
        let rc = home.join(".zshrc");
        let bin = home.join("cargo-bin");
        let (c1, _) = append_path_line(&rc, &bin).unwrap();
        assert!(c1);
        let (c2, _) = append_path_line(&rc, &bin).unwrap();
        assert!(!c2, "ikinci çağrı değişiklik yapmamalı");
        let content = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(content.matches("# snappdf").count(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn append_path_line_preserves_existing_content() {
        let home = temp_home("preserve");
        let rc = home.join(".bashrc");
        std::fs::write(&rc, "export EDITOR=vim").unwrap();
        let bin = home.join("cargo-bin");
        append_path_line(&rc, &bin).unwrap();
        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(content.starts_with("export EDITOR=vim"));
        assert!(content.ends_with('\n'));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn is_on_path_true_when_dir_matches() {
        let home = temp_home("onpath");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // PATH'i doğrudan kuramayız (kopya ikili çalıştırma sorunu), ama
        // split_paths davranışını simüle ederiz: env manupülasyonu testte
        // güvenlidir çünkü process-local.
        let old = std::env::var_os("PATH");
        // SAFETY: test süresince process-local PATH değişimi.
        unsafe { std::env::set_var("PATH", &bin) };
        assert!(is_on_path(&bin));
        match old {
            Some(o) => unsafe { std::env::set_var("PATH", o) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn is_on_path_false_for_missing_dir() {
        let home = temp_home("nopath");
        let bin = home.join("yok-bin");
        let old = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", &bin) };
        assert!(!is_on_path(&bin));
        match old {
            Some(o) => unsafe { std::env::set_var("PATH", o) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn binary_installed_detects_file() {
        let home = temp_home("bininst");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(!binary_installed(&bin));
        std::fs::write(bin.join("snappdf"), "#!/bin/sh\n").unwrap();
        assert!(binary_installed(&bin));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn perform_install_dry_run_lists_steps_without_cargo() {
        // Dry-run modu cargo'yu çalıştırmaz; plan listesi döner.
        let steps = perform_install_with(true).unwrap();
        assert!(steps.len() >= 3);
        assert!(steps[0].contains("önizleme"));
        assert!(steps.iter().any(|s| s.contains("PATH")));
    }

    #[test]
    fn home_dir_error_message_contains_hint() {
        // HOME her zaman var (test ortamı); fonksiyonun hata yolu yine de
        // doğrulanabilir: HOME'u boşaltıp context mesajını kontrol et.
        let old = std::env::var_os("HOME");
        unsafe { std::env::remove_var("HOME") };
        let err = home_dir().unwrap_err().to_string();
        if let Some(o) = old {
            unsafe { std::env::set_var("HOME", o) };
        }
        assert!(err.contains("HOME"));
    }

    #[test]
    fn append_path_line_handles_missing_parent_gracefully() {
        // rc dosyasının klasörü yoksa yazma hatası net bir anyhow hatası olmalı.
        let missing = std::env::temp_dir().join("snappdf-yok-klasor").join("rc");
        let bin = std::env::temp_dir().join("snappdf-yok-bin");
        let err = append_path_line(&missing, &bin).unwrap_err().to_string();
        assert!(err.contains("yazılamadı"));
    }
}
