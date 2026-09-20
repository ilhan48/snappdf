//! Kod blokları için hafif söz dizimi renklendirme.
//!
//! Tarayıcısız çalıştığımız ve ağır ayrıştırıcı bağımlılıkları istemediğimiz
//! için tam bir dil bilgisi yerine küçük bir tarayıcı kullanılır: yorumlar,
//! dizeler, sayılar, anahtar sözcükler, türler ve işlev çağrıları tanınır;
//! geri kalanı düz metin kalır. Amaç bloglardaki gibi *okunur* bir kod
//! görünümü, dilbilimsel doğruluk değil.
//!
//! Girdi metni tek bir turda taranır, çıktı ise satır satır simge listesidir
//! (PDF'te her kod satırı ayrı bir paragraf olur).

/// Bir simgenin türü; PDF katmanı bunu renge çevirir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ayrıştırılamayan her şey: operatörler, boşluklar, değişken adları.
    Plain,
    /// `fn`, `def`, `class`, `if` gibi anahtar sözcükler.
    Keyword,
    /// Büyük harfle başlayan adlar (`FontData`, `MyStruct`).
    Type,
    /// Çağrılan adlar (`println!`, `render(...)`).
    Function,
    /// Dize/sablon değişmezi.
    Str,
    /// Yorum.
    Comment,
    /// Sayı.
    Number,
}

/// Renklendirilmiş tek bir metin parçası.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    /// Özgün metinden alınan parça (boşluklar dâhil).
    pub text: &'a str,
    /// Parçanın türü.
    pub kind: Kind,
}

/// Bir dilin tanıma kuralları.
struct Spec {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Dize başlatan karakterler.
    quotes: &'static str,
    /// Üçlü tırnak (Python `"""..."""`) desteklenir mi?
    triple_quotes: bool,
    /// Büyük harfle başlayan adlar tür sayılır mı?
    capital_types: bool,
    /// Anahtar sözcükler büyük/küçük harf duyarsız mı (SQL, HTML)?
    case_insensitive: bool,
    /// HTML/XML etiket modu.
    markup: bool,
}

/// Kod bloğunu satır satır simgelere ayırır.
///
/// `lang` `None` ise (veya tanınmıyorsa) yaygın kural kümesi kullanılır.
pub fn highlight<'a>(code: &'a str, lang: Option<&str>) -> Vec<Vec<Token<'a>>> {
    let spec = spec_for(lang);
    let mut lines: Vec<Vec<Token<'a>>> = vec![Vec::new()];
    let mut i = 0usize;

    while i < code.len() {
        let rest = &code[i..];
        let c = rest.chars().next().unwrap();

        // Satır yorumu: satır sonuna kadar. Satır sonu tüketilmez; bir sonraki
        // turda düz metin olarak basılır ve satırı böler.
        if spec.line_comments.iter().any(|m| rest.starts_with(*m)) {
            let len = rest.find('\n').unwrap_or(rest.len());
            push_lines(&mut lines, &rest[..len], Kind::Comment);
            i += len;
            continue;
        }

        // Blok yorumu (birden fazla satıra yayılabilir).
        if let Some((open, close)) = spec.block_comment {
            if let Some(after) = rest.strip_prefix(open) {
                let len = after
                    .find(close)
                    .map(|k| k + close.len())
                    .unwrap_or(after.len());
                push_lines(&mut lines, &rest[..open.len() + len], Kind::Comment);
                i += open.len() + len;
                continue;
            }
        }

        // Dizeler.
        if spec.quotes.contains(c) {
            let len = scan_string(rest, c, spec.triple_quotes);
            push_lines(&mut lines, &rest[..len], Kind::Str);
            i += len;
            continue;
        }

        // HTML/XML etiketleri.
        if spec.markup && c == '<' {
            i += scan_tag(rest, &mut lines);
            continue;
        }

        // Sayılar.
        if c.is_ascii_digit() {
            let len = scan_number(rest);
            push_lines(&mut lines, &rest[..len], Kind::Number);
            i += len;
            continue;
        }

        // Adlar (anahtar sözcük, tür, işlev ya da düz metin).
        if is_word_start(c) {
            let len = scan_word(rest).max(c.len_utf8());
            let word = &rest[..len];
            push_lines(&mut lines, word, classify(&spec, word, &rest[len..]));
            i += len;
            continue;
        }

        // Geri kalan her şey (boşluk, operatör, noktalama): düz metin.
        // Ardışık düz karakterler tek simgede toplanır (daha az stil geçişi).
        let mut len = c.len_utf8();
        while i + len < code.len() {
            let next = &code[i + len..];
            let ch = next.chars().next().unwrap();
            if is_word_start(ch)
                || ch.is_ascii_digit()
                || spec.quotes.contains(ch)
                || (spec.markup && ch == '<')
                || starts_marker(&spec, next)
            {
                break;
            }
            len += ch.len_utf8();
        }
        push_lines(&mut lines, &rest[..len], Kind::Plain);
        i += len;
    }

    lines
}

/// Bir simgeyi satırlara böler: metindeki `\n` yeni satır açar.
fn push_lines<'a>(lines: &mut Vec<Vec<Token<'a>>>, text: &'a str, kind: Kind) {
    if text.is_empty() {
        return;
    }
    let mut parts = text.split('\n');
    if let Some(first) = parts.next() {
        if !first.is_empty() {
            lines.last_mut().unwrap().push(Token { text: first, kind });
        }
    }
    for part in parts {
        lines.push(Vec::new());
        if !part.is_empty() {
            lines.last_mut().unwrap().push(Token { text: part, kind });
        }
    }
}

/// Verilen noktadan bir yorum işareti (satır ya da blok) başlıyor mu?
fn starts_marker(spec: &Spec, rest: &str) -> bool {
    spec.line_comments.iter().any(|m| rest.starts_with(*m))
        || spec
            .block_comment
            .is_some_and(|(open, _)| rest.starts_with(open))
}

/// Ad başlatan karakter mi? (`$` ve `@`: PHP/Bash değişkenleri, dekoratörler)
fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$' || c == '@'
}

/// Adın uzunluğu. İlk karakter zaten geçerli kabul edilir.
fn scan_word(s: &str) -> usize {
    let mut end = s.chars().next().map(char::len_utf8).unwrap_or(0);
    for (idx, ch) in s.char_indices().skip(1) {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Adı sınıflandırır. `after`, adın hemen ardından gelen metindir (çağrı
/// algısı için gerekir).
fn classify(spec: &Spec, word: &str, after: &str) -> Kind {
    if is_keyword(spec, word) {
        return Kind::Keyword;
    }
    if word.starts_with('@') {
        // Python/Java dekoratörleri, PHP değişkenleri.
        return Kind::Type;
    }
    if spec.capital_types
        && word
            .chars()
            .next()
            .map(char::is_uppercase)
            .unwrap_or(false)
    {
        return Kind::Type;
    }
    let rest = after.trim_start_matches([' ', '\t']);
    // Çağrı: `foo(` ya da Rust makroları `foo!(`.
    let call = rest.starts_with('(')
        || (rest.starts_with('!') && rest[1..].trim_start_matches(' ').starts_with('('));
    if call {
        return Kind::Function;
    }
    Kind::Plain
}

/// Anahtar sözcük karşılaştırması (dil gerekirse harf duyarsız).
fn is_keyword(spec: &Spec, word: &str) -> bool {
    if spec.case_insensitive {
        spec.keywords
            .iter()
            .any(|kw| kw.eq_ignore_ascii_case(word))
    } else {
        spec.keywords.contains(&word)
    }
}

/// Sayının uzunluğu: `0x1F`, `1_000`, `3.14`, `1e-9`, `2.5f64`.
fn scan_number(s: &str) -> usize {
    let b = s.as_bytes();
    let hex = b.len() > 1 && b[0] == b'0' && matches!(b[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O');
    let mut i = if hex { 2 } else { 0 };
    let mut fractional = false;

    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() || c == b'_' || (hex && c.is_ascii_hexdigit()) {
            i += 1;
        } else if c == b'.' && !fractional {
            // `1..5` (Rust aralığı) sayı değildir.
            if b.get(i + 1) == Some(&b'.') {
                break;
            }
            fractional = true;
            i += 1;
        } else if matches!(c, b'e' | b'E') && !hex && starts_number(&b[i + 1..]) {
            i += 1;
            if matches!(b.get(i), Some(b'+') | Some(b'-')) {
                i += 1;
            }
        } else {
            break;
        }
    }

    // Tür soneki (`42u8`, `1.5f64`, `10L`) — kısa ve harfle başlıyorsa.
    let start = i;
    while i < b.len() && b[i].is_ascii_alphanumeric() {
        i += 1;
    }
    if i == start || i - start > 3 {
        i = start;
    }
    i.max(1)
}

/// `+`/`-` sonrası basamak ya da doğrudan basamak: üs işareti kontrolü.
fn starts_number(rest: &[u8]) -> bool {
    match rest.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'+') | Some(b'-') => rest.get(1).is_some_and(u8::is_ascii_digit),
        _ => false,
    }
}

/// Dizenin uzunluğu. Kaçış (`\`) ve üçlü tırnak desteklenir; tek satırlık
/// dizeler satır sonunda biter.
fn scan_string(s: &str, quote: char, triple_ok: bool) -> usize {
    let qlen = quote.len_utf8();
    if triple_ok {
        let triple: String = std::iter::repeat_n(quote, 3).collect();
        if s.starts_with(&triple) {
            let after = &s[triple.len()..];
            return match after.find(&triple) {
                Some(k) => triple.len() + k + triple.len(),
                None => s.len(),
            };
        }
    }
    // Backtick şablon dizeleri satır aşabilir.
    let multiline = quote == '`';
    let mut i = qlen;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        if c == '\\' {
            i += c.len_utf8();
            if let Some(next) = s[i..].chars().next() {
                i += next.len_utf8();
            }
            continue;
        }
        if c == quote {
            i += qlen;
            break;
        }
        if c == '\n' && !multiline {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// `<...>` etiketini (ve içindeki öznitelikleri) simgelere ayırır.
///
/// Etiket adı anahtar sözcük, öznitelik adları tür, değerler dize rengi alır;
/// böylece HTML/XML örnekleri de bloglardaki gibi görünür.
fn scan_tag<'a>(s: &'a str, lines: &mut Vec<Vec<Token<'a>>>) -> usize {
    if let Some(after) = s.strip_prefix("<!--") {
        let len = after.find("-->").map(|k| k + 7).unwrap_or(s.len());
        push_lines(lines, &s[..len], Kind::Comment);
        return len;
    }

    // Etiketin sonu: tırnak içindeki `>` sayılmaz.
    let b = s.as_bytes();
    let mut end = s.len();
    let mut quote: Option<u8> = None;
    let mut k = 1;
    while k < b.len() {
        let c = b[k];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => {
                end = k + 1;
                break;
            }
            None => {}
        }
        k += 1;
    }
    let tag = &s[..end];

    push_lines(lines, &tag[..1], Kind::Plain);
    let mut i = 1;
    if tag[i..].starts_with('/') {
        push_lines(lines, "/", Kind::Plain);
        i += 1;
    }
    let name_start = i;
    while i < tag.len() && is_tag_char(tag.as_bytes()[i]) {
        i += 1;
    }
    if i > name_start {
        push_lines(lines, &tag[name_start..i], Kind::Keyword);
    }

    let tb = tag.as_bytes();
    while i < tb.len() {
        let c = tb[i];
        if c == b'"' || c == b'\'' {
            let len = scan_string(&tag[i..], c as char, false);
            push_lines(lines, &tag[i..i + len], Kind::Str);
            i += len;
        } else if is_tag_char(c) {
            let start = i;
            while i < tb.len() && is_tag_char(tb[i]) {
                i += 1;
            }
            push_lines(lines, &tag[start..i], Kind::Type);
        } else {
            push_lines(lines, &tag[i..i + 1], Kind::Plain);
            i += 1;
        }
    }
    end
}

/// Etiket/öznitelik adında geçebilen ASCII karakterler.
fn is_tag_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':' | b'.' | b'@' | b'#' | b'*' | b'$')
}

/// Dile göre kural kümesi. Tanınmayan dilde yaygın kurallar kullanılır.
fn spec_for(lang: Option<&str>) -> Spec {
    let key = lang.map(normalize_lang).unwrap_or_default();
    match key.as_str() {
        "rust" => Spec {
            keywords: RUST_KW,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
        "python" => Spec {
            keywords: PYTHON_KW,
            line_comments: &["#"],
            block_comment: None,
            quotes: "\"'",
            triple_quotes: true,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "javascript" | "typescript" => Spec {
            keywords: JS_KW,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"'`",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
        "go" => Spec {
            keywords: GO_KW,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"`",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "c" | "cpp" | "java" | "csharp" | "kotlin" | "swift" | "scala" => Spec {
            keywords: CLIKE_KW,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"'",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
        "bash" => Spec {
            keywords: SHELL_KW,
            line_comments: &["#"],
            block_comment: None,
            quotes: "\"'",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "sql" => Spec {
            keywords: SQL_KW,
            line_comments: &["--"],
            block_comment: Some(("/*", "*/")),
            quotes: "'\"",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: true,
            markup: false,
        },
        "ruby" => Spec {
            keywords: RUBY_KW,
            line_comments: &["#"],
            block_comment: None,
            quotes: "\"'",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
        "php" => Spec {
            keywords: PHP_KW,
            line_comments: &["//", "#"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"'",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
        "json" => Spec {
            keywords: DATA_KW,
            line_comments: &[],
            block_comment: None,
            quotes: "\"",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "yaml" | "toml" | "ini" => Spec {
            keywords: DATA_KW,
            line_comments: &["#", ";"],
            block_comment: None,
            quotes: "\"'",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "css" => Spec {
            keywords: &[],
            line_comments: &[],
            block_comment: Some(("/*", "*/")),
            quotes: "\"'",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "markdown" => Spec {
            keywords: &[],
            line_comments: &[],
            block_comment: None,
            quotes: "\"",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: false,
        },
        "html" => Spec {
            keywords: &[],
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            quotes: "\"'",
            triple_quotes: false,
            capital_types: false,
            case_insensitive: false,
            markup: true,
        },
        _ => Spec {
            keywords: GENERIC_KW,
            line_comments: &["//", "#"],
            block_comment: Some(("/*", "*/")),
            quotes: "\"'`",
            triple_quotes: false,
            capital_types: true,
            case_insensitive: false,
            markup: false,
        },
    }
}

/// Tanınan dillerin kanonik adları.
const KNOWN_LANGUAGES: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "go",
    "c",
    "cpp",
    "java",
    "csharp",
    "kotlin",
    "swift",
    "scala",
    "bash",
    "sql",
    "ruby",
    "php",
    "json",
    "yaml",
    "toml",
    "ini",
    "css",
    "markdown",
    "html",
];

/// Ad (takma adlar dâhil) tanınan bir dil mi? `class="rust"` gibi çıplak dil
/// belirteçlerini kabul etmek için kullanılır.
pub fn is_known_language(name: &str) -> bool {
    KNOWN_LANGUAGES.contains(&normalize_lang(name).as_str())
}

/// Sınıf/öznitelik değerlerinde dil adını başlatan önekler
/// (`language-rust`, `highlight-source-python` ...).
pub const LANGUAGE_PREFIXES: &[&str] = &[
    "language-",
    "lang-",
    "highlight-source-",
    "highlight-",
    "brush-",
    "prism-",
    "syntax-",
];

/// Dil ipucunu kanonik anahtara indirger (`Language-Rust`, `py`, `js` ...).
pub fn normalize_lang(lang: &str) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    let lang = LANGUAGE_PREFIXES
        .iter()
        .find_map(|prefix| lang.strip_prefix(prefix))
        .unwrap_or(&lang);
    match lang {
        "rs" => "rust".to_string(),
        "py" | "python3" | "py3" | "sage" => "python".to_string(),
        "js" | "jsx" | "node" | "mjs" | "cjs" => "javascript".to_string(),
        "ts" | "tsx" => "typescript".to_string(),
        "golang" => "go".to_string(),
        "c++" | "cxx" | "cc" | "hpp" | "hxx" => "cpp".to_string(),
        "h" => "c".to_string(),
        "cs" | "c#" => "csharp".to_string(),
        "sh" | "shell" | "zsh" | "console" | "shell-session" | "terminal" | "ksh" => {
            "bash".to_string()
        }
        "mysql" | "postgres" | "postgresql" | "sqlite" | "plsql" | "tsql" => "sql".to_string(),
        "rb" => "ruby".to_string(),
        "yml" => "yaml".to_string(),
        "jsonc" | "json5" => "json".to_string(),
        "kt" | "kts" => "kotlin".to_string(),
        "scss" | "sass" | "less" | "styl" => "css".to_string(),
        "xml" | "xhtml" | "svg" | "rss" | "vue" | "html5" => "html".to_string(),
        "md" | "mdx" => "markdown".to_string(),
        other => other.to_string(),
    }
}


const RUST_KW: &[&str] = &[
    "as", "async", "await", "bool", "break", "char", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "f32", "f64", "false", "fn", "for", "i128", "i16", "i32", "i64",
    "i8", "if", "impl", "in", "isize", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "str", "struct", "super", "trait", "true",
    "type", "u128", "u16", "u32", "u64", "u8", "unsafe", "use", "usize", "where", "while",
    "yield",
];

const PYTHON_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "self", "try", "while", "with", "yield",
    "True", "False", "None",
];

const JS_KW: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "export", "extends", "finally", "for", "from", "function",
    "get", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "null",
    "of", "private", "protected", "public", "readonly", "return", "set", "static", "super",
    "switch", "this", "throw", "try", "type", "typeof", "undefined", "var", "void", "while",
    "yield", "true", "false",
];

const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
    "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range", "return",
    "select", "struct", "switch", "type", "var", "nil", "true", "false",
];

const CLIKE_KW: &[&str] = &[
    "abstract", "as", "assert", "auto", "bool", "break", "case", "catch", "char", "class",
    "const", "continue", "data", "default", "defer", "delete", "do", "double", "else", "enum",
    "extends", "extern", "false", "final", "finally", "float", "fn", "for", "fun", "func",
    "guard", "if", "implements", "import", "in", "inline", "instanceof", "int", "interface",
    "internal", "is", "lambda", "let", "long", "map", "namespace", "new", "nil", "null",
    "nullptr", "object", "operator", "override", "package", "private", "protected", "protocol",
    "public", "readonly", "register", "repeat", "return", "sealed", "short", "signed", "sizeof",
    "static", "struct", "super", "switch", "template", "this", "throw", "throws", "trait", "true",
    "try", "typealias", "typedef", "typename", "union", "unsafe", "unsigned", "val", "var",
    "virtual", "void", "volatile", "when", "where", "while", "with", "yield",
];

const SHELL_KW: &[&str] = &[
    "alias", "break", "case", "cd", "continue", "do", "done", "echo", "elif", "else", "esac",
    "exec", "exit", "export", "fi", "for", "function", "if", "in", "local", "readonly", "return",
    "select", "set", "shift", "source", "then", "trap", "typeset", "unset", "until", "while",
];

const SQL_KW: &[&str] = &[
    "add", "all", "alter", "and", "any", "as", "asc", "avg", "begin", "between", "by", "case",
    "cast", "check", "column", "commit", "constraint", "count", "create", "cross", "current",
    "database", "default", "delete", "desc", "distinct", "drop", "else", "end", "except",
    "exists", "false", "foreign", "from", "full", "group", "having", "if", "in", "index", "inner",
    "insert", "into", "is", "join", "key", "left", "like", "limit", "max", "min", "not", "null",
    "offset", "on", "or", "order", "outer", "primary", "references", "right", "rollback", "select",
    "set", "sum", "table", "then", "true", "union", "unique", "update", "values", "view", "when",
    "where", "with",
];

const RUBY_KW: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
    "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo",
    "require", "rescue", "retry", "return", "self", "super", "then", "true", "undef", "unless",
    "until", "when", "while", "yield",
];

const PHP_KW: &[&str] = &[
    "abstract", "and", "array", "as", "break", "case", "catch", "class", "clone", "const",
    "continue", "declare", "default", "do", "echo", "else", "elseif", "enddeclare", "endfor",
    "endforeach", "endif", "endswitch", "endwhile", "extends", "final", "finally", "fn", "for",
    "foreach", "function", "global", "if", "implements", "include", "instanceof", "interface",
    "isset", "list", "match", "namespace", "new", "or", "print", "private", "protected", "public",
    "readonly", "require", "return", "static", "switch", "throw", "trait", "try", "unset", "use",
    "var", "while", "xor", "yield", "true", "false", "null",
];

const DATA_KW: &[&str] = &["true", "false", "null", "none", "True", "False", "None", "yes", "no"];

const GENERIC_KW: &[&str] = &[
    "and", "as", "async", "await", "break", "case", "class", "const", "continue", "def",
    "default", "do", "else", "elseif", "end", "enum", "except", "export", "extends", "false",
    "finally", "fn", "for", "from", "func", "function", "if", "impl", "import", "in",
    "interface",
    "let", "match", "new", "nil", "none", "not", "null", "of", "or", "package", "pass", "private",
    "protected", "pub", "public", "raise", "return", "self", "static", "struct", "switch", "then",
    "this", "throw", "trait", "true", "try", "type", "use", "var", "void", "when", "where",
    "while", "with", "yield",
];

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Simgeleri birleştirince özgün metin birebir çıkmalı (kayıp/tekrar yok).
    fn rebuilt(code: &str, lang: Option<&str>) -> String {
        highlight(code, lang)
            .iter()
            .map(|line| line.iter().map(|t| t.text).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Bir satırın simge türleri.
    fn kinds_of(code: &str, lang: Option<&str>, line: usize) -> Vec<Kind> {
        highlight(code, lang)[line].iter().map(|t| t.kind).collect()
    }

    fn kind_of(code: &str, lang: Option<&str>, needle: &str) -> Kind {
        highlight(code, lang)
            .iter()
            .flatten()
            .find(|t| t.text == needle)
            .unwrap_or_else(|| panic!("{needle} simgesi bulunamadı"))
            .kind
    }

    #[test]
    fn round_trip_preserves_the_source_exactly() {
        let rust = "fn main() {\n    // yorum\n    let s = \"a\\nb\";\n    println!(\"{s}\", 1.5f64);\n}\n";
        let python = "# yorum\n\"\"\"çok\nsatırlı\"\"\"\ndef f(x):\n    return x + 1\n";
        let html = "<div class=\"a\">\n  <!-- yorum -->\n  <p id='x'>metin</p>\n</div>\n";
        let sql = "SELECT * FROM t WHERE a = 'b'; -- yorum\n";
        for (code, lang) in [
            (rust, Some("rust")),
            (python, Some("python")),
            (html, Some("html")),
            (sql, Some("sql")),
            ("a b\tc\n\nson", None),
        ] {
            assert_eq!(rebuilt(code, lang), code, "{lang:?}");
        }
    }

    #[test]
    fn line_count_matches_source_lines() {
        let code = "let a = 1;\n\nlet b = 2;";
        assert_eq!(highlight(code, Some("rust")).len(), 3);
        // Boş satır simgesiz kalır (PDF katmanı boşluğu kendisi basar).
        assert!(highlight(code, Some("rust"))[1].is_empty());
    }

    #[test]
    fn rust_tokens_are_classified() {
        let code = "// açıklama\nfn main() { let x: u32 = 42; println!(\"hi\"); }";
        assert_eq!(kind_of(code, Some("rust"), "// açıklama"), Kind::Comment);
        assert_eq!(kind_of(code, Some("rust"), "fn"), Kind::Keyword);
        assert_eq!(kind_of(code, Some("rust"), "let"), Kind::Keyword);
        // İlkel türler de anahtar sözcük rengini alır (`u32`, `bool` ...).
        assert_eq!(kind_of(code, Some("rust"), "u32"), Kind::Keyword);
        assert_eq!(kind_of(code, Some("rust"), "42"), Kind::Number);
        assert_eq!(kind_of(code, Some("rust"), "\"hi\""), Kind::Str);
        assert_eq!(kind_of(code, Some("rust"), "println"), Kind::Function);
    }

    #[test]
    fn string_escape_does_not_end_the_string() {
        let code = r#"let s = "a\"b";"#;
        assert_eq!(kind_of(code, Some("rust"), r#""a\"b""#), Kind::Str);
        assert_eq!(rebuilt(code, Some("rust")), code);
    }

    #[test]
    fn python_comments_hashes_and_triple_strings() {
        let code = "# yorum\ndef f():\n    \"\"\"doc\n    string\"\"\"\n    return 1";
        assert_eq!(kind_of(code, Some("python"), "# yorum"), Kind::Comment);
        assert_eq!(kind_of(code, Some("python"), "def"), Kind::Keyword);
        // Üçlü dize iki satıra yayılır: girinti düz, kalanı dize renginde.
        assert_eq!(kinds_of(code, Some("python"), 2), vec![Kind::Plain, Kind::Str]);
        assert_eq!(kinds_of(code, Some("python"), 3), vec![Kind::Str]);
        // `return` anahtar sözcük, ardından gelen `1` sayı.
        assert_eq!(kind_of(code, Some("python"), "1"), Kind::Number);
    }

    #[test]
    fn capitalize_is_not_a_type_in_python() {
        assert_eq!(kind_of("Foo = 1", Some("python"), "Foo"), Kind::Plain);
        assert_eq!(kind_of("let x = Foo;", Some("rust"), "Foo"), Kind::Type);
    }

    #[test]
    fn block_comments_span_lines_and_win_over_code() {
        let code = "/* burada\nlet x = 1;\n*/\nlet y = 2;";
        assert_eq!(kinds_of(code, Some("rust"), 0), vec![Kind::Comment]);
        assert_eq!(kinds_of(code, Some("rust"), 1), vec![Kind::Comment]);
        assert_eq!(kind_of(code, Some("rust"), "let"), Kind::Keyword);
        assert_eq!(rebuilt(code, Some("rust")), code);
    }

    #[test]
    fn sql_keywords_are_case_insensitive() {
        assert_eq!(kind_of("SELECT a FROM t", Some("sql"), "SELECT"), Kind::Keyword);
        assert_eq!(kind_of("select a from t", Some("sql"), "from"), Kind::Keyword);
        // Rust'ta `select` anahtar sözcük değil.
        assert_eq!(kind_of("select a from t", Some("rust"), "select"), Kind::Plain);
    }

    #[test]
    fn html_tags_and_attributes_are_colored() {
        let code = "<a href=\"/x\" class=\"y\">tık</a>";
        assert_eq!(kind_of(code, Some("html"), "<"), Kind::Plain);
        assert_eq!(kind_of(code, Some("html"), "a"), Kind::Keyword);
        assert_eq!(kind_of(code, Some("html"), "href"), Kind::Type);
        assert_eq!(kind_of(code, Some("html"), "\"/x\""), Kind::Str);
        assert_eq!(kind_of(code, Some("html"), "tık"), Kind::Plain);
        assert_eq!(kind_of(code, Some("html"), "/"), Kind::Plain);
    }

    #[test]
    fn number_scanner_handles_common_forms() {
        assert_eq!(kind_of("x = 0xFF", Some("rust"), "0xFF"), Kind::Number);
        assert_eq!(kind_of("x = 1_000_000", Some("rust"), "1_000_000"), Kind::Number);
        assert_eq!(kind_of("x = 1.5e-3", Some("rust"), "1.5e-3"), Kind::Number);
        assert_eq!(kind_of("x = 42u8", Some("rust"), "42u8"), Kind::Number);
        // Aralık operatörü sayıya yapışmaz.
        assert_eq!(kind_of("for i in 0..10 {}", Some("rust"), "0"), Kind::Number);
        assert_eq!(rebuilt("for i in 0..10 {}", Some("rust")), "for i in 0..10 {}");
    }

    #[test]
    fn unknown_language_falls_back_to_generic_rules() {
        let code = "# yorum\nfunc main() { say(\"hi\") }";
        assert_eq!(kind_of(code, None, "# yorum"), Kind::Comment);
        assert_eq!(kind_of(code, None, "func"), Kind::Keyword);
        assert_eq!(kind_of(code, None, "say"), Kind::Function);
        assert_eq!(kind_of(code, Some("hiç-duyulmamış"), "\"hi\""), Kind::Str);
    }

    #[test]
    fn known_languages_are_recognized_by_alias() {
        assert!(is_known_language("rust"));
        assert!(is_known_language("py"));
        assert!(is_known_language("golang"));
        assert!(is_known_language("c++"));
        assert!(!is_known_language("hljs"));
        assert!(!is_known_language("highlight"));
        assert!(!is_known_language("text"));
        assert!(!is_known_language(""));
    }

    #[test]
    fn normalize_lang_maps_aliases() {
        assert_eq!(normalize_lang("Language-Rust"), "rust");
        assert_eq!(normalize_lang("py"), "python");
        assert_eq!(normalize_lang("JS"), "javascript");
        assert_eq!(normalize_lang("c++"), "cpp");
        assert_eq!(normalize_lang("shell"), "bash");
        assert_eq!(normalize_lang("yml"), "yaml");
        assert_eq!(normalize_lang("bilinmeyen"), "bilinmeyen");
    }

    #[test]
    fn empty_input_yields_one_empty_line() {
        assert_eq!(highlight("", Some("rust")).len(), 1);
        assert!(highlight("", Some("rust"))[0].is_empty());
    }
}
