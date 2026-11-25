use crate::schema::SEARCH_COLUMNS;

pub(crate) fn search_text_expr() -> String {
    search_text_expr_with_prefix("")
}

pub(crate) fn search_text_expr_with_prefix(prefix: &str) -> String {
    let mut parts: Vec<String> = SEARCH_COLUMNS
        .iter()
        .map(|c| format!("COALESCE({prefix}{c},'')"))
        .collect();
    parts.push(format!("COALESCE(extra_values_text({prefix}extra),'')"));
    parts.join(" || ' ' || ")
}

/// Builds an FTS5 query from user input.
/// Each word is an exact phrase; `word*` performs prefix search.
/// Numeric terms with 4+ digits are automatically treated as prefixes,
/// which is convenient for product codes.
pub fn build_fts_query(input: &str) -> String {
    fn flush(terms: &mut Vec<String>, current: &mut String, prefix: bool) {
        if current.is_empty() {
            return;
        }
        let all_digits = current.chars().all(|c| c.is_ascii_digit());
        let prefix = prefix || (all_digits && current.len() >= 4);
        let star = if prefix { "*" } else { "" };
        terms.push(format!("\"{current}\"{star}"));
        current.clear();
    }
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in input.chars() {
        if terms.len() >= 32 {
            break;
        }
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if ch == '*' {
            flush(&mut terms, &mut current, true);
        } else {
            flush(&mut terms, &mut current, false);
        }
    }
    flush(&mut terms, &mut current, false);
    terms.join(" ")
}

/// Prefix FTS terms for a filter value: `JYSK Ukraine` -> `"jysk"* "ukraine"*`.
/// Returns None when the value cannot produce reliable terms, such as 1-char tokens.
pub fn fts_prefix_terms(value: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            if current.chars().count() >= 3 {
                terms.push(format!("\"{current}\"*"));
            }
            current.clear();
        }
        if terms.len() >= 8 {
            break;
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Allocation-free case-insensitive substring search, including Cyrillic text.
/// `needle_lower` must already be lowercased.
pub fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let Some(first) = needle_lower.chars().next() else {
        return true;
    };
    for (i, c) in hay.char_indices() {
        if c.to_lowercase().next() != Some(first) {
            continue;
        }
        let mut h = hay[i..].chars().flat_map(char::to_lowercase);
        let mut n = needle_lower.chars();
        loop {
            let Some(nc) = n.next() else {
                return true;
            };
            if h.next() != Some(nc) {
                break;
            }
        }
    }
    false
}

pub(crate) fn plain_search_terms(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '\'' || ch == '-'))
        .filter_map(|term| {
            let term = term.trim_matches(['*', '\'', '-']).to_lowercase();
            (term.chars().count() >= 2).then_some(term)
        })
        .take(32)
        .collect()
}

pub(crate) fn glob_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '*' | '?' | '[' | ']' => {
                out.push('[');
                out.push(ch);
                out.push(']');
            }
            _ => out.push(ch),
        }
    }
    out
}

pub(crate) fn product_code_search_prefix(value: &str) -> Option<&str> {
    let value = value.trim();
    if !(4..=10).contains(&value.len()) || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let chapter = value.get(..2)?.parse::<u8>().ok()?;
    if !(1..=97).contains(&chapter) {
        return None;
    }
    if value.len() == 4 && value.starts_with("20") {
        return None;
    }
    Some(value)
}
