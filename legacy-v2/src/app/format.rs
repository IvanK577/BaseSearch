use crate::db::AnalyticsMeasures;
use crate::i18n::{Lang, tr};

/// "2024-03" -> "03'24".
pub(super) fn short_month(month: &str) -> String {
    match (month.get(0..4), month.get(5..7)) {
        (Some(year), Some(m)) => format!("{m}'{}", &year[2..]),
        _ => month.to_string(),
    }
}

/// Compact number for chart captions: 12.4M, 980K, 312.
pub(super) fn fmt_compact(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1.0e9 {
        format!("{:.1}B", value / 1.0e9)
    } else if abs >= 1.0e6 {
        format!("{:.1}M", value / 1.0e6)
    } else if abs >= 1.0e4 {
        format!("{:.0}K", value / 1.0e3)
    } else if abs >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

pub(super) fn fmt_decimal(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.decimals$}");
    if let Some(dot) = s.find('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.len() == dot + 1 {
            s.pop();
        }
    }
    let (sign, body) = s
        .strip_prefix('-')
        .map(|rest| ("-", rest))
        .unwrap_or(("", s.as_str()));
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let mut grouped = String::with_capacity(s.len() + s.len() / 3);
    grouped.push_str(sign);
    for (i, ch) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i).is_multiple_of(3) {
            grouped.push('\u{202F}');
        }
        grouped.push(ch);
    }
    if !frac_part.is_empty() {
        grouped.push('.');
        grouped.push_str(frac_part);
    }
    grouped
}

/// Money for a stat tile, carrying the currency it is denominated in.
///
/// The desktop used to print `SUM(value)` with no label at all, which is the
/// right number only while every row happens to share a currency. When they do
/// not, no scalar is true of them, and saying so is the only honest option —
/// the per-currency breakdown is in `measures.currency_totals`.
pub fn fmt_money_compact(measures: &AnalyticsMeasures, lang: Lang) -> String {
    label_money(
        measures.single_currency_total(),
        measures.currency_totals.is_empty(),
        lang,
        fmt_compact,
    )
}

/// Money per kilogram on the same rule.
pub fn fmt_money_per_kg(measures: &AnalyticsMeasures, lang: Lang) -> String {
    label_money(
        measures.single_currency_per_net_kg(),
        measures.value_per_net_weight.is_empty(),
        lang,
        |value| fmt_decimal(value, 2),
    )
}

/// Money at full precision, for report tables and tooltips.
pub fn fmt_money_exact(measures: &AnalyticsMeasures, lang: Lang) -> String {
    label_money(
        measures.single_currency_total(),
        measures.currency_totals.is_empty(),
        lang,
        |value| fmt_decimal(value, 2),
    )
}

fn label_money(
    figure: Option<(f64, &str)>,
    nothing_to_show: bool,
    lang: Lang,
    render: impl Fn(f64) -> String,
) -> String {
    match figure {
        // A single bucket whose currency the source never stated: show the
        // number bare, as it has always been shown, and let the standing
        // currency note explain it.
        Some((value, "")) => render(value),
        Some((value, code)) => format!("{} {code}", render(value)),
        // No money in these rows at all is a zero, not a currency conflict.
        None if nothing_to_show => render(0.0),
        None => tr(lang).mixed_currencies.to_string(),
    }
}

/// Every currency in these rows with its own total: `1.0M USD · 480K EUR`.
///
/// `None` when there is nothing to explain — no money at all, or one currency,
/// which the figure itself already carries. This is what turns "several
/// currencies" from a refusal into an answer. The person came to read numbers,
/// and the numbers exist; it is only their sum that does not.
pub fn currency_breakdown(measures: &AnalyticsMeasures, lang: Lang) -> Option<String> {
    if measures.currency_totals.len() < 2 {
        return None;
    }
    let unstated = tr(lang).unstated_currency;
    Some(
        measures
            .currency_totals
            .iter()
            .map(|total| {
                let code = if total.known {
                    total.currency.as_str()
                } else {
                    unstated
                };
                format!("{} {code}", fmt_compact(total.total_value))
            })
            .collect::<Vec<_>>()
            .join("  ·  "),
    )
}

/// Value per document, which is only meaningful inside a single currency.
pub(super) fn value_per_document(measures: &AnalyticsMeasures, documents: u64) -> Option<f64> {
    let (total, _) = measures.single_currency_total()?;
    (documents > 0).then(|| total / documents as f64)
}

#[cfg(test)]
mod tests {
    use super::{
        currency_breakdown, fmt_compact, fmt_decimal, fmt_money_compact, fmt_money_exact,
        short_month, value_per_document,
    };
    use crate::db::{AnalyticsCurrencyTotal, AnalyticsMeasures};
    use crate::i18n::Lang;

    fn bucket(currency: &str, known: bool, total: f64) -> AnalyticsCurrencyTotal {
        AnalyticsCurrencyTotal {
            currency: currency.to_string(),
            known,
            valued_rows: 1,
            total_value: total,
        }
    }

    fn measures(totals: Vec<AnalyticsCurrencyTotal>) -> AnalyticsMeasures {
        AnalyticsMeasures {
            currency_totals: totals,
            ..Default::default()
        }
    }

    #[test]
    fn a_single_bucket_shows_its_number_with_its_currency() {
        let usd = measures(vec![bucket("USD", true, 1500.0)]);
        assert_eq!(fmt_money_compact(&usd, Lang::En), "1500 USD");
        assert_eq!(fmt_money_exact(&usd, Lang::En), "1\u{202F}500 USD");
        assert_eq!(value_per_document(&usd, 3), Some(500.0));
    }

    #[test]
    fn an_unstated_currency_still_shows_the_number() {
        let unknown = measures(vec![bucket("__unknown__", false, 1500.0)]);
        assert_eq!(fmt_money_compact(&unknown, Lang::En), "1500");
        assert_eq!(value_per_document(&unknown, 3), Some(500.0));
    }

    #[test]
    fn several_currencies_never_collapse_into_one_number() {
        let mixed = measures(vec![
            bucket("USD", true, 1000.0),
            bucket("EUR", true, 500.0),
        ]);
        assert_eq!(fmt_money_compact(&mixed, Lang::En), "Several currencies");
        assert_eq!(fmt_money_exact(&mixed, Lang::En), "Several currencies");
        assert!(
            !fmt_money_compact(&mixed, Lang::En).contains("1500"),
            "the two buckets must never be added together"
        );
        assert_eq!(
            value_per_document(&mixed, 3),
            None,
            "a per-document average across currencies is not a number"
        );
    }

    /// Refusing to add two currencies is only half an answer. The other half
    /// is telling the person which two, and how much of each — otherwise the
    /// figures they came for have simply disappeared.
    #[test]
    fn several_currencies_are_then_shown_one_by_one() {
        let mixed = measures(vec![
            bucket("USD", true, 1000.0),
            bucket("EUR", true, 500.0),
        ]);
        assert_eq!(
            currency_breakdown(&mixed, Lang::En).as_deref(),
            Some("1000 USD  ·  500 EUR")
        );
    }

    #[test]
    fn a_bucket_without_a_stated_currency_is_named_as_such() {
        let mixed = measures(vec![
            bucket("USD", true, 1000.0),
            bucket("__unknown__", false, 500.0),
        ]);
        assert_eq!(
            currency_breakdown(&mixed, Lang::En).as_deref(),
            Some("1000 USD  ·  500 currency not stated")
        );
    }

    /// One currency needs no breakdown: the figure beside it already says so,
    /// and repeating it would be noise on every ordinary workspace.
    #[test]
    fn one_currency_has_nothing_to_break_down() {
        assert_eq!(
            currency_breakdown(&measures(vec![bucket("USD", true, 1000.0)]), Lang::En),
            None
        );
        assert_eq!(currency_breakdown(&measures(vec![]), Lang::En), None);
    }

    #[test]
    fn no_money_at_all_is_a_zero_not_a_currency_conflict() {
        assert_eq!(fmt_money_compact(&measures(vec![]), Lang::En), "0.0");
    }

    #[test]
    fn short_month_compacts_iso_month() {
        assert_eq!(short_month("2024-03"), "03'24");
        assert_eq!(short_month("bad"), "bad");
    }

    #[test]
    fn compact_numbers_match_existing_ui_scale() {
        assert_eq!(fmt_compact(12_400_000.0), "12.4M");
        assert_eq!(fmt_compact(980_000.0), "980K");
        assert_eq!(fmt_compact(312.0), "312");
        assert_eq!(fmt_compact(9.25), "9.2");
    }

    #[test]
    fn decimals_trim_zeroes_and_group_thousands() {
        assert_eq!(fmt_decimal(1234.50, 2), "1\u{202F}234.5");
        assert_eq!(fmt_decimal(-1234567.0, 2), "-1\u{202F}234\u{202F}567");
        assert_eq!(fmt_decimal(f64::NAN, 2), "0");
    }
}
