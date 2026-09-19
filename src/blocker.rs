use adblock::blocker::BlockerResult;
use adblock::engine::Engine;
use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use anyhow::Result;

/// EasyList/EasyPrivacy motoru (Brave'ın kullandığı adblock-rust).
pub struct Blocker {
    engine: Engine,
}

impl Blocker {
    /// Birleştirilmiş filtre listesi metninden motoru derler.
    pub fn new(combined_rules: String) -> Result<Self> {
        let mut set = FilterSet::new(false);
        set.add_filter_list(combined_rules, ParseOptions::default());
        let engine = Engine::new_with_filter_set(set);
        Ok(Self { engine })
    }

    /// İstek engellenmeli mi? (adblock-rust'a "subframe" isteği olarak sorar)
    pub fn should_block(&self, request_url: &str, source_url: &str, resource_type: &str) -> bool {
        // adblock-rust URL tipini kendisi algılar; `method` bazı kurallarda
        // ($method opt) kullanıldığından GET varsayıyoruz.
        let req = match Request::new(request_url, source_url, resource_type, "GET") {
            Ok(r) => r,
            Err(_) => return false, // çözümlenemeyen URL'ye dokunma
        };
        let res: BlockerResult = self.engine.check_network_request(&req);
        res.should_block()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "\
||doubleclick.net^
||googlesyndication.com^
||google-analytics.com^
||facebook.net/tr/tr/fbevents.js$script
@@||goodcdn.example^$image
/banner-ad-$image
";

    fn blocker() -> Blocker {
        Blocker::new(RULES.to_string()).unwrap()
    }

    #[test]
    fn engine_compiles_from_rules() {
        let _ = blocker(); // panik yok = derleme başarılı
    }

    #[test]
    fn blocks_ad_domain() {
        let b = blocker();
        assert!(b.should_block(
            "https://ad.doubleclick.net/ddm/adj/x",
            "https://site.com/yazi",
            "script"
        ));
    }

    #[test]
    fn blocks_analytics() {
        let b = blocker();
        assert!(b.should_block(
            "https://www.google-analytics.com/analytics.js",
            "https://site.com/yazi",
            "script"
        ));
    }

    #[test]
    fn blocks_syndication_subresource() {
        let b = blocker();
        assert!(b.should_block(
            "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js",
            "https://site.com/yazi",
            "script"
        ));
    }

    #[test]
    fn allows_normal_content() {
        let b = blocker();
        assert!(!b.should_block(
            "https://cdn.site.com/app.js",
            "https://site.com/yazi",
            "script"
        ));
        assert!(!b.should_block(
            "https://site.com/styles.css",
            "https://site.com/yazi",
            "stylesheet"
        ));
    }

    #[test]
    fn exception_rule_whitelists() {
        let b = blocker();
        assert!(!b.should_block(
            "https://goodcdn.example/pic.png",
            "https://site.com/yazi",
            "image"
        ));
    }

    #[test]
    fn unparseable_url_is_allowed() {
        let b = blocker();
        // Ayrıştırılamayan girdiler engellenmez (fail-open sözleşmesi).
        assert!(!b.should_block("not a url at all", "also bad", "other"));
        assert!(!b.should_block("", "", ""));
    }

    #[test]
    fn empty_rules_block_nothing() {
        let b = Blocker::new(String::new()).unwrap();
        assert!(!b.should_block("https://ad.doubleclick.net/x", "https://site.com", "script"));
    }

    #[test]
    fn should_block_is_deterministic() {
        let b = blocker();
        let url = "https://ad.doubleclick.net/ddm/adj/x";
        let first = b.should_block(url, "https://site.com", "script");
        let second = b.should_block(url, "https://site.com", "script");
        assert_eq!(first, second);
    }
}
