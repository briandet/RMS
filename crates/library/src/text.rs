//! Text normalisation helpers, ported from `Slim::Utils::Text`, `Slim::Music::Info::splitTag`
//! and `Slim::Utils::Unicode::utf8toLatin1Transliterate`.

use regex::Regex;

/// Holds the compiled article regex (pref `ignoredarticles`) and the split list (pref `splitList`).
#[derive(Clone, Debug)]
pub struct TextRules {
    articles: Option<Regex>,
    split_list: Vec<String>,
}

impl TextRules {
    pub fn new(ignored_articles: &str, split_list: &str) -> Self {
        let words: Vec<String> = ignored_articles
            .split_whitespace()
            .map(regex::escape)
            .collect();
        let articles = if words.is_empty() {
            None
        } else {
            Regex::new(&format!(r"(?i)^({})\s+", words.join("|"))).ok()
        };
        TextRules {
            articles,
            split_list: split_list.split_whitespace().map(|s| s.to_string()).collect(),
        }
    }

    /// `ignoreArticles`: strip one leading article (case-insensitive) followed by whitespace.
    pub fn ignore_articles(&self, s: &str) -> String {
        match &self.articles {
            Some(re) => re.replace(s, "").into_owned(),
            None => s.to_string(),
        }
    }

    /// `ignoreCaseArticles($s, $transliterate, $keepArticles)`
    pub fn ignore_case_articles(&self, s: &str, transliterate: bool, keep_articles: bool) -> String {
        // Perl: `if (!$s) { return $s }` - empty string and "0" are returned unchanged
        if s.is_empty() || s == "0" {
            return s.to_string();
        }
        let mut value = s.to_uppercase();
        if !keep_articles {
            value = self.ignore_articles(&value);
        }
        value = ignore_punct(&value);
        // strip characters beyond U+FFFF
        value.retain(|c| (c as u32) < 0x10000);
        let value = value.trim_matches(' ').to_string();
        if transliterate {
            transliterate_latin1(&value)
        } else {
            value
        }
    }

    /// Value stored in `*sort` columns.
    pub fn sort_key(&self, s: &str) -> String {
        self.ignore_case_articles(s, false, false)
    }

    /// Value stored in `*search` columns (`ignoreCase($s, 1)`).
    pub fn search_key(&self, s: &str) -> String {
        self.ignore_case_articles(s, true, true)
    }

    /// `searchStringSplit`: the normalised search string (articles stripped, transliterated).
    pub fn normalize_search(&self, s: &str) -> String {
        self.ignore_case_articles(s, true, false)
    }

    /// `Slim::Music::Info::splitTag` for a single (non-array) tag value.
    pub fn split_tag(&self, tag: &str) -> Vec<String> {
        split_tag_with(tag, &self.split_list)
    }

    /// splitTag for a tag that may be multi-valued (Vorbis arrays are not split further).
    pub fn split_values(&self, values: &[String]) -> Vec<String> {
        if values.len() > 1 {
            values
                .iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect()
        } else if let Some(v) = values.first() {
            self.split_tag(v)
                .into_iter()
                .filter(|v| !v.is_empty())
                .collect()
        } else {
            Vec::new()
        }
    }
}

pub fn split_tag_with(tag: &str, separators: &[String]) -> Vec<String> {
    let re_rb = regex_rb();
    if re_rb.0.is_match(tag) || re_rb.1.is_match(tag) {
        return vec![tag.to_string()];
    }
    let mut split_tags: Vec<String> = Vec::new();
    if !separators.is_empty() {
        let mut seps: Vec<&str> = separators.iter().map(|s| s.as_str()).collect();
        seps.push("\0");
        for sep in seps {
            let temp: Vec<String> = tag
                .split(sep)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.trim().is_empty())
                .collect();
            if temp.len() > 1 {
                split_tags.extend(temp);
            }
        }
    } else {
        // "\x00" is always a separator in practice for ID3v2.4 multi-values
        let temp: Vec<String> = tag
            .split('\0')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if temp.len() > 1 {
            split_tags.extend(temp);
        }
    }
    if split_tags.len() > 1 {
        return split_tags;
    }
    vec![tag.trim().to_string()]
}

fn regex_rb() -> &'static (Regex, Regex) {
    use std::sync::OnceLock;
    static RE: OnceLock<(Regex, Regex)> = OnceLock::new();
    RE.get_or_init(|| {
        (
            Regex::new(r"(?i)^\s*R\s*&\s*B\s*$").unwrap(),
            Regex::new(r"(?i)^\s*Rock\s*&\s*Roll\s*$").unwrap(),
        )
    })
}

/// Perl `[[:punct:]]` on a Unicode string: Unicode punctuation (\p{P}) plus the ASCII
/// symbol characters that POSIX counts as punctuation.
pub fn is_punct(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_punctuation();
    }
    let u = c as u32;
    matches!(u,
        0x00A1 | 0x00A7 | 0x00AB | 0x00B6 | 0x00B7 | 0x00BB | 0x00BF
        | 0x037E | 0x0387
        | 0x055A..=0x055F | 0x0589 | 0x058A
        | 0x05BE | 0x05C0 | 0x05C3 | 0x05C6 | 0x05F3 | 0x05F4
        | 0x0609 | 0x060A | 0x060C | 0x060D | 0x061B | 0x061E | 0x061F | 0x066A..=0x066D | 0x06D4
        | 0x0964 | 0x0965 | 0x0970
        | 0x0E4F | 0x0E5A | 0x0E5B
        | 0x2010..=0x2027 | 0x2030..=0x2043 | 0x2045..=0x2051 | 0x2053..=0x205E
        | 0x207D | 0x207E | 0x208D | 0x208E
        | 0x2308..=0x230B | 0x2329 | 0x232A
        | 0x2768..=0x2775 | 0x27C5 | 0x27C6 | 0x27E6..=0x27EF | 0x2983..=0x2998 | 0x29D8..=0x29DB | 0x29FC | 0x29FD
        | 0x2E00..=0x2E4F
        | 0x3001..=0x3003 | 0x3008..=0x3011 | 0x3014..=0x301F | 0x3030 | 0x303D | 0x30A0 | 0x30FB
        | 0xFE10..=0xFE19 | 0xFE30..=0xFE52 | 0xFE54..=0xFE61 | 0xFE63 | 0xFE68 | 0xFE6A | 0xFE6B
        | 0xFF01..=0xFF03 | 0xFF05..=0xFF0A | 0xFF0C..=0xFF0F | 0xFF1A | 0xFF1B | 0xFF1F | 0xFF20
        | 0xFF3B..=0xFF3D | 0xFF3F | 0xFF5B | 0xFF5D | 0xFF5F..=0xFF65
    )
}

/// `ignorePunct`: punctuation runs → single space, collapse spaces, trim; revert if empty.
pub fn ignore_punct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_punct = false;
    for c in s.chars() {
        if is_punct(c) {
            if !in_punct {
                out.push(' ');
            }
            in_punct = true;
        } else {
            in_punct = false;
            out.push(c);
        }
    }
    // compact multiple spaces
    let mut compact = String::with_capacity(out.len());
    let mut prev_space = false;
    for c in out.chars() {
        if c == ' ' {
            if !prev_space {
                compact.push(c);
            }
            prev_space = true;
        } else {
            prev_space = false;
            compact.push(c);
        }
    }
    let trimmed = compact.trim_matches(' ');
    if trimmed.is_empty() {
        s.to_string()
    } else {
        trimmed.to_string()
    }
}

/// `utf8toLatin1Transliterate`: Text::Unidecode then Latin-1 (i.e. plain ASCII).
pub fn transliterate_latin1(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let out = deunicode::deunicode_with_tofu(s, "?");
    out
}

/// `matchCase`: upper case and fold Latin-1 diacritics, Æ→AE, µ→MU.
pub fn match_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'Æ' | 'æ' => out.push_str("AE"),
            'µ' => out.push_str("MU"),
            'ß' => out.push('B'),
            'À'..='Å' | 'à'..='å' => out.push('A'),
            'Ç' | 'ç' => out.push('C'),
            'È'..='Ë' | 'è'..='ë' => out.push('E'),
            'Ì'..='Ï' | 'ì'..='ï' => out.push('I'),
            'Ñ' | 'ñ' => out.push('N'),
            'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => out.push('O'),
            'Ù'..='Ü' | 'ù'..='ü' => out.push('U'),
            'Ý' | 'ý' | 'ÿ' => out.push('Y'),
            '×' => out.push('X'),
            'Ð' | 'ð' => out.push('D'),
            c if c.is_ascii_lowercase() => out.push(c.to_ascii_uppercase()),
            c => out.push(c),
        }
    }
    out
}

/// Comparison key for the `perllocale` collation: accent-insensitive first, then exact.
pub fn collate_key(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_uppercase()
}

pub fn collate_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    collate_key(a).cmp(&collate_key(b)).then_with(|| a.cmp(b))
}

/// First character (as string) – `substr($x, 0, 1)`.
pub fn first_char(s: &str) -> String {
    s.chars().next().map(|c| c.to_string()).unwrap_or_default()
}

/// URI::Escape::uri_escape_utf8 – escape everything but RFC3986 unreserved chars.
pub fn uri_escape_utf8(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
    const SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
    utf8_percent_encode(s, SET).to_string()
}

/// Clean a replay gain value the LMS way: strip " dB", whitespace, ',' -> '.', must be numeric.
pub fn clean_gain(v: &str) -> Option<f64> {
    let re = Regex::new(r"(?i)\s*dB").unwrap();
    let mut s = re.replace_all(v, "").to_string();
    s.retain(|c| !c.is_whitespace());
    let s = s.replace(',', ".");
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '+' || c == '.') {
        return None;
    }
    s.parse::<f64>().ok()
}

/// `cleanTrackNumber`: first run of digits, 0 → None.
pub fn clean_track_number(v: &str) -> Option<i64> {
    let digits: String = v
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    match digits.parse::<i64>() {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// `sanitizeYearTag`: first 4-digit run.
pub fn sanitize_year(v: &str) -> Option<String> {
    let re = Regex::new(r"(\d\d\d\d)").unwrap();
    // Perl: s/.*(\d\d\d\d).*/$1/ is greedy: the *last* 4-digit run wins
    let mut last = None;
    for c in re.captures_iter(v) {
        last = Some(c[1].to_string());
    }
    // the greedy `.*` prefix makes Perl pick the last possible match position
    match last {
        Some(y) => Some(y),
        None => Some(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_and_search_keys() {
        let r = TextRules::new("The El La Los Las Le Les", ";");
        assert_eq!(r.sort_key("The Beatles"), "BEATLES");
        assert_eq!(r.search_key("The Beatles"), "THE BEATLES");
        assert_eq!(r.sort_key("Les Misérables"), "MISÉRABLES");
        assert_eq!(r.search_key("Björk"), "BJORK");
        assert_eq!(r.sort_key("L.A. Woman"), "L A WOMAN");
        assert_eq!(r.sort_key("!!!"), "!!!");
        assert_eq!(r.normalize_search("the beatles"), "BEATLES");
    }

    #[test]
    fn split() {
        let r = TextRules::new("The", ";");
        assert_eq!(r.split_tag("A; B"), vec!["A", "B"]);
        assert_eq!(r.split_tag("R&B"), vec!["R&B"]);
        assert_eq!(r.split_tag(" Solo "), vec!["Solo"]);
        let r = TextRules::new("The", "; /");
        assert_eq!(r.split_tag("A / B"), vec!["A", "B"]);
    }

    #[test]
    fn misc() {
        assert_eq!(clean_track_number("3/12"), Some(3));
        assert_eq!(clean_track_number("0"), None);
        assert_eq!(clean_gain("-6.5 dB"), Some(-6.5));
        assert_eq!(clean_gain("-6,5dB"), Some(-6.5));
        assert_eq!(clean_gain("abc"), None);
        assert_eq!(match_case("björk"), "BJORK");
        assert_eq!(uri_escape_utf8("A B&C"), "A%20B%26C");
    }
}
