/// Preferred reading of the ambiguous "1-3 digits, one separator, exactly
/// three trailing digits" form (`1.250` / `1,250`), which is a thousands
/// group in some locales and a three-decimal value in others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberStyle {
    /// `1.250` reads as `1.25` — right for weights and per-unit prices,
    /// where three decimal places are standard.
    PreferDecimal,
    /// `1.250` reads as `1250` — right for totals and quantities, where a
    /// lone separator followed by exactly three digits is almost always a
    /// thousands group.
    PreferGrouped,
}

pub fn parse_number(value: &str) -> Option<f64> {
    parse_number_styled(value, NumberStyle::PreferDecimal)
}

pub fn parse_number_grouped(value: &str) -> Option<f64> {
    parse_number_styled(value, NumberStyle::PreferGrouped)
}

pub fn parse_number_styled(value: &str, style: NumberStyle) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(number) = parse_scientific(value) {
        return Some(number);
    }
    // A value that looks like scientific notation (a digit next to `e`/`E`)
    // but failed the strict parse above is not a number — e.g. "5e3 kg".
    // Falling through would strip the letters and concatenate the digits into
    // a silently wrong integer (53), so reject it outright.
    if looks_like_failed_scientific(value) {
        return None;
    }

    let core = numeric_core(value)?;
    let unsigned = core.strip_prefix(['+', '-']).unwrap_or(core);
    let sign = core
        .starts_with('-')
        .then_some('-')
        .or_else(|| core.starts_with('+').then_some('+'));
    let has_digit_groups = unsigned.chars().any(is_digit_group_separator);

    let dot_count = unsigned.matches('.').count();
    let comma_count = unsigned.matches(',').count();
    let decimal_sep = match (dot_count, comma_count) {
        (0, 0) => None,
        (0, 1) => single_separator_decimal(unsigned, ',', has_digit_groups, style),
        (1, 0) => single_separator_decimal(unsigned, '.', has_digit_groups, style),
        (0, _) | (_, 0) => None,
        _ => {
            let last_dot = unsigned.rfind('.').unwrap_or(0);
            let last_comma = unsigned.rfind(',').unwrap_or(0);
            Some(if last_dot > last_comma { '.' } else { ',' })
        }
    };

    let (integer, fraction) = if let Some(separator) = decimal_sep {
        if unsigned.matches(separator).count() != 1 {
            return None;
        }
        let position = unsigned.rfind(separator)?;
        let fraction = &unsigned[position + separator.len_utf8()..];
        if fraction.is_empty() || !fraction.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        (&unsigned[..position], Some(fraction))
    } else {
        (unsigned, None)
    };

    let integer_digits = normalize_grouped_integer(integer, fraction.is_some())?;
    let mut normalized = String::with_capacity(core.len());
    if let Some(sign) = sign {
        normalized.push(sign);
    }
    normalized.push_str(&integer_digits);
    if let Some(fraction) = fraction {
        normalized.push('.');
        normalized.push_str(fraction);
    }

    normalized
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

fn numeric_core(value: &str) -> Option<&str> {
    let (first_digit, _) = value.char_indices().find(|(_, ch)| ch.is_ascii_digit())?;
    let (last_digit, last) = value.char_indices().rfind(|(_, ch)| ch.is_ascii_digit())?;
    let mut start = first_digit;

    if let Some((index, ch)) = value[..start].char_indices().next_back() {
        if matches!(ch, '.' | ',') {
            start = index;
            if let Some((sign_index, sign)) = value[..start].char_indices().next_back()
                && matches!(sign, '+' | '-')
            {
                start = sign_index;
            }
        } else if matches!(ch, '+' | '-') {
            start = index;
        }
    }

    let end = last_digit + last.len_utf8();
    if !valid_numeric_affix(&value[..start]) || !valid_numeric_affix(&value[end..]) {
        return None;
    }

    let core = &value[start..end];
    for (index, ch) in core.chars().enumerate() {
        let valid = ch.is_ascii_digit()
            || matches!(ch, '.' | ',')
            || is_digit_group_separator(ch)
            || (index == 0 && matches!(ch, '+' | '-'));
        if !valid {
            return None;
        }
    }
    Some(core)
}

fn valid_numeric_affix(value: &str) -> bool {
    !value.chars().any(|ch| {
        ch.is_ascii_digit()
            || matches!(ch, '.' | ',' | '+' | '-')
            || matches!(ch, '\'' | '\u{2019}')
    })
}

fn is_digit_group_separator(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\u{00A0}' | '\u{202F}' | '\u{2009}' | '\'' | '\u{2019}'
    )
}

fn normalize_grouped_integer(value: &str, allow_empty: bool) -> Option<String> {
    if value.is_empty() {
        return allow_empty.then(String::new);
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(value.to_string());
    }

    let groups = value
        .split(|ch: char| matches!(ch, '.' | ',') || is_digit_group_separator(ch))
        .collect::<Vec<_>>();
    if groups.len() < 2
        || groups[0].is_empty()
        || groups[0].len() > 3
        || groups
            .iter()
            .any(|group| !group.chars().all(|ch| ch.is_ascii_digit()))
        || groups[1..].iter().any(|group| group.len() != 3)
    {
        return None;
    }
    Some(groups.concat())
}

pub(crate) fn parse_year(value: &str) -> Option<i64> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 4 {
        digits.parse().ok()
    } else {
        None
    }
}

pub(crate) fn month_key(value: &str) -> String {
    let trimmed = value.trim();
    let date = trimmed.split([' ', 'T']).next().unwrap_or(trimmed);
    let parts: Vec<&str> = date.split(['.', '/', '-']).collect();
    if parts.len() == 3 {
        if parts[0].len() <= 2
            && parts[1].len() <= 2
            && parts[2].len() == 4
            && let (Ok(_d), Ok(m), Ok(y)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
            && (1..=12).contains(&m)
        {
            return format!("{y:04}-{m:02}");
        }
        if parts[0].len() <= 2
            && parts[1].len() <= 2
            && parts[2].len() == 4
            && let (Ok(m), Ok(_d), Ok(y)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
            && (1..=12).contains(&m)
        {
            return format!("{y:04}-{m:02}");
        }
        if parts[0].len() == 4
            && parts[1].len() <= 2
            && parts[2].len() <= 2
            && let (Ok(y), Ok(m), Ok(_d)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
            && (1..=12).contains(&m)
        {
            return format!("{y:04}-{m:02}");
        }
    }
    String::new()
}

pub(crate) fn normalize_country_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let key: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect();
    if matches!(
        key.as_str(),
        "0" | "00" | "000" | "NA" | "NODATA" | "ND" | "НД" | "НЕМАДАНИХ" | "НЕТДАННЫХ"
    ) {
        return String::new();
    }
    match key.as_str() {
        "CN" | "CHN" | "CHINA" | "КИТАЙ" => "CN",
        "IE" | "IRL" | "IRELAND" | "ІРЛАНДІЯ" | "ИРЛАНДИЯ" => "IE",
        "PL" | "POL" | "POLAND" | "ПОЛЬЩА" | "ПОЛЬША" => "PL",
        "CZ"
        | "CZE"
        | "CZECHIA"
        | "CZECHREPUBLIC"
        | "ЧЕСЬКАРЕСПУБЛІКА"
        | "ЧЕХІЯ"
        | "ЧЕШСКАЯРЕСПУБЛИКА"
        | "ЧЕХИЯ" => "CZ",
        "DE" | "DEU" | "GERMANY" | "НІМЕЧЧИНА" | "ГЕРМАНІЯ" | "ГЕРМАНИЯ" => {
            "DE"
        }
        "US"
        | "USA"
        | "UNITEDSTATES"
        | "UNITEDSTATESOFAMERICA"
        | "СПОЛУЧЕНІШТАТИАМЕРИКИ"
        | "США"
        | "СОЕДИНЕННЫЕШТАТЫАМЕРИКИ" => "US",
        "VN" | "VNM" | "VIETNAM" | "ВЄТНАМ" | "ВЕТНАМ" => "VN",
        "EU" | "EUROPEANUNION" | "КРАЇНИЄС" | "СТРАНЫЕС" => "EU",
        "KR"
        | "KOR"
        | "SOUTHKOREA"
        | "REPUBLICOFKOREA"
        | "ПІВДЕННАКОРЕЯ"
        | "КОРЕЯРЕСПУБЛІКА"
        | "ЮЖНАЯКОРЕЯ" => "KR",
        "TR" | "TUR" | "TURKEY" | "TURKIYE" | "ТУРЕЧЧИНА" | "ТУРЦІЯ" | "ТУРЦИЯ" => {
            "TR"
        }
        "IN" | "IND" | "INDIA" | "ІНДІЯ" | "ИНДИЯ" => "IN",
        "IT" | "ITA" | "ITALY" | "ІТАЛІЯ" | "ИТАЛИЯ" => "IT",
        "BE" | "BEL" | "BELGIUM" | "БЕЛЬГІЯ" | "БЕЛЬГИЯ" => "BE",
        "NL" | "NLD" | "NETHERLANDS" | "НІДЕРЛАНДИ" | "НИДЕРЛАНДЫ" => "NL",
        "FR" | "FRA" | "FRANCE" | "ФРАНЦІЯ" | "ФРАНЦИЯ" => "FR",
        "GB"
        | "UK"
        | "GBR"
        | "GREATBRITAIN"
        | "UNITEDKINGDOM"
        | "ВЕЛИКАБРИТАНІЯ"
        | "ВЕЛИКОБРИТАНІЯ"
        | "ВЕЛИКОБРИТАНИЯ" => "GB",
        "ES" | "ESP" | "SPAIN" | "ІСПАНІЯ" | "ИСПАНИЯ" => "ES",
        "CH" | "CHE" | "SWITZERLAND" | "ШВЕЙЦАРІЯ" | "ШВЕЙЦАРИЯ" => "CH",
        "AT" | "AUT" | "AUSTRIA" | "АВСТРІЯ" | "АВСТРИЯ" => "AT",
        "FI" | "FIN" | "FINLAND" | "ФІНЛЯНДІЯ" | "ФИНЛЯНДИЯ" => "FI",
        "LV" | "LVA" | "LATVIA" | "ЛАТВІЯ" | "ЛАТВИЯ" => "LV",
        "LT" | "LTU" | "LITHUANIA" | "ЛИТВА" => "LT",
        "EE" | "EST" | "ESTONIA" | "ЕСТОНІЯ" | "ЭСТОНИЯ" => "EE",
        "HU" | "HUN" | "HUNGARY" | "УГОРЩИНА" | "ВЕНГРИЯ" => "HU",
        "RO" | "ROU" | "ROMANIA" | "РУМУНІЯ" | "РУМЫНИЯ" => "RO",
        "BG" | "BGR" | "BULGARIA" | "БОЛГАРІЯ" | "БОЛГАРИЯ" => "BG",
        _ => return key,
    }
    .to_string()
}

pub(crate) fn normalize_text_key(value: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for ch in value.trim().chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            last_space = false;
        }
    }
    out
}

pub(crate) fn clean_label_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let key: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect();
    if matches!(
        key.as_str(),
        "0" | "00"
            | "000"
            | "0000"
            | "NA"
            | "NА"
            | "ND"
            | "NULL"
            | "NONE"
            | "NODATA"
            | "UNKNOWN"
            | "НД"
            | "НЕМАДАНИХ"
            | "НЕТДАННЫХ"
            | "НЕВІДОМО"
            | "НЕИЗВЕСТНО"
    ) {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Extracts a bounded 19xx/20xx year from date text.
pub fn extract_year(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    for window_start in 0..bytes.len().saturating_sub(3) {
        let w = &bytes[window_start..window_start + 4];
        let plausible_year = ((w[0] == b'1' && w[1] == b'9') || (w[0] == b'2' && w[1] == b'0'))
            && w[2].is_ascii_digit()
            && w[3].is_ascii_digit();
        if plausible_year {
            let before_digit = window_start > 0 && bytes[window_start - 1].is_ascii_digit();
            let after_digit =
                window_start + 4 < bytes.len() && bytes[window_start + 4].is_ascii_digit();
            if !before_digit && !after_digit {
                return std::str::from_utf8(w).ok()?.parse().ok();
            }
        }
    }
    None
}

fn single_separator_decimal(
    value: &str,
    sep: char,
    has_group_spaces: bool,
    style: NumberStyle,
) -> Option<char> {
    let pos = value.rfind(sep)?;
    let after = value[pos + sep.len_utf8()..]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .count();
    if after == 0 {
        return None;
    }
    if after != 3 {
        return Some(sep);
    }
    // Exactly three digits follow the lone separator — the classic
    // thousands-vs-decimals ambiguity ("1.250"). Decisive signals first:
    // space groups mean the separator is decimal; a >3-digit or zero-led
    // integer part cannot be the first thousands group.
    let integer: Vec<char> = value[..pos]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let leading_zero = integer.first().is_some_and(|c| *c == '0');
    if has_group_spaces || integer.len() > 3 || integer.is_empty() || leading_zero {
        return Some(sep);
    }
    match style {
        NumberStyle::PreferDecimal => Some(sep),
        NumberStyle::PreferGrouped => None,
    }
}

/// Strict scientific-notation form. Excel renders large numbers to text as
/// `1.23E+08`; the manual separator logic would otherwise drop the exponent
/// and produce a silently wrong value. Anything looser than
/// `[+-]digits[.,digits][eE][+-]digits` falls back to the localized parser.
fn parse_scientific(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let (mantissa, exponent) = trimmed.split_once(['e', 'E'])?;
    if mantissa.is_empty() || exponent.is_empty() {
        return None;
    }
    let exp_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    if exp_digits.is_empty() || !exp_digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mantissa_body = mantissa.strip_prefix(['+', '-']).unwrap_or(mantissa);
    if !mantissa_body.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut seen_sep = false;
    for ch in mantissa_body.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        if matches!(ch, '.' | ',') && !seen_sep {
            seen_sep = true;
            continue;
        }
        return None;
    }
    trimmed
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

/// True when an exponent marker touches a digit but `parse_scientific`
/// rejected the complete value. Such strings ("5e3 kg", "1e", "e3") must
/// not fall through to the localized parser as an unrelated plain number.
fn looks_like_failed_scientific(value: &str) -> bool {
    let chars: Vec<char> = value.trim().chars().collect();
    chars.iter().enumerate().any(|(i, ch)| {
        if *ch != 'e' && *ch != 'E' {
            return false;
        }
        // A digit just before the exponent marker.
        let digit_before = i > 0 && chars[i - 1].is_ascii_digit();
        // A digit (optionally after a sign) just after it.
        let after = &chars[i + 1..];
        let after = match after.first() {
            Some('+') | Some('-') => &after[1..],
            _ => after,
        };
        let digit_after = after.first().is_some_and(|c| c.is_ascii_digit());
        digit_before || digit_after
    })
}
