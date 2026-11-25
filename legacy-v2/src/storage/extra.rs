use std::collections::HashSet;

use crate::storage::normalize::normalize_text_key;

/// Parses the stored `extra` JSON array of `[header, value]` pairs.
pub(crate) fn parse_extra(raw: Option<&str>) -> Vec<(String, String)> {
    raw.and_then(|text| serde_json::from_str::<Vec<(String, String)>>(text).ok())
        .unwrap_or_default()
}

pub(crate) fn remember_extra_header(
    seen: &mut HashSet<String>,
    headers: &mut Vec<String>,
    header: &str,
) {
    let header = header.trim();
    let key = normalize_text_key(header);
    if !key.is_empty() && seen.insert(key) {
        headers.push(header.to_string());
    }
}

pub(crate) fn extra_value_for_header(raw: Option<&str>, header: Option<&str>) -> String {
    let Some(header) = header else {
        return String::new();
    };
    let wanted = normalize_text_key(header);
    if wanted.is_empty() {
        return String::new();
    }
    parse_extra(raw)
        .into_iter()
        .find_map(|(candidate, value)| (normalize_text_key(&candidate) == wanted).then_some(value))
        .unwrap_or_default()
}
