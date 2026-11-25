use serde::{Deserialize, Serialize};

use crate::db::{Filters, Query};
use crate::i18n::{Lang, Tr, fmt, tr};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredQuery {
    pub name: String,
    pub query: Query,
}

#[cfg(test)]
pub(super) fn encode_stored_queries(items: &[StoredQuery]) -> String {
    items
        .iter()
        .map(|item| {
            let f = &item.query.filters;
            [
                item.name.as_str(),
                item.query.text.as_str(),
                f.year.as_str(),
                f.product_code.as_str(),
                f.trademark.as_str(),
                f.description.as_str(),
                f.sender.as_str(),
                f.recipient.as_str(),
                f.edrpou.as_str(),
                f.trade_country.as_str(),
                f.dispatch_country.as_str(),
                f.origin_country.as_str(),
            ]
            .iter()
            .map(|value| encode_component(value))
            .collect::<Vec<_>>()
            .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn decode_stored_queries(raw: &str) -> Vec<StoredQuery> {
    raw.lines()
        .filter_map(|line| {
            let fields = line
                .split('\t')
                .map(decode_component)
                .collect::<Option<Vec<_>>>()?;
            if fields.len() != 12 {
                return None;
            }
            let query = Query {
                text: fields[1].clone(),
                filters: Filters {
                    year: fields[2].clone(),
                    product_code: fields[3].clone(),
                    trademark: fields[4].clone(),
                    description: fields[5].clone(),
                    sender: fields[6].clone(),
                    recipient: fields[7].clone(),
                    edrpou: fields[8].clone(),
                    trade_country: fields[9].clone(),
                    dispatch_country: fields[10].clone(),
                    origin_country: fields[11].clone(),
                },
                advanced: None,
                record_scope: Default::default(),
            };
            if query.is_empty() {
                return None;
            }
            let name = if fields[0].trim().is_empty() {
                legacy_query_summary(&query, tr(Lang::En))
            } else {
                fields[0].clone()
            };
            Some(StoredQuery { name, query })
        })
        .collect()
}

pub(super) fn encode_stored_queries_v2(items: &[StoredQuery]) -> String {
    serde_json::to_string(items).unwrap_or_default()
}

pub(super) fn decode_stored_queries_v2(raw: &str) -> Vec<StoredQuery> {
    serde_json::from_str::<Vec<StoredQuery>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| !item.query.is_empty())
        .collect()
}

pub(super) fn decode_stored_queries_with_fallback(
    v2_raw: Option<String>,
    v1_raw: Option<String>,
) -> Vec<StoredQuery> {
    if let Some(raw) = v2_raw
        && !raw.trim().is_empty()
    {
        let decoded = decode_stored_queries_v2(&raw);
        if !decoded.is_empty() {
            return decoded;
        }
    }
    v1_raw
        .as_deref()
        .map(decode_stored_queries)
        .unwrap_or_default()
}

#[cfg(test)]
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(ch),
        }
    }
    out
}

fn decode_component(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = &value[i + 1..i + 3];
            match hex {
                "25" => out.push('%'),
                "09" => out.push('\t'),
                "0A" => out.push('\n'),
                "0D" => out.push('\r'),
                _ => return None,
            }
            i += 3;
        } else {
            let ch = value[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Some(out)
}

fn legacy_query_summary(query: &Query, t: &Tr) -> String {
    if query.is_empty() {
        return t.enter_query_hint.to_string();
    }
    let f = &query.filters;
    let mut parts = Vec::new();
    if !query.text.trim().is_empty() {
        parts.push(query.text.trim().to_string());
    }
    for (label, value) in [
        (t.year, &f.year),
        (t.product_code, &f.product_code),
        (t.trademark, &f.trademark),
        (t.description, &f.description),
        (t.sender, &f.sender),
        (t.recipient, &f.recipient),
        (t.edrpou, &f.edrpou),
        (t.trade_country, &f.trade_country),
        (t.dispatch_country, &f.dispatch_country),
        (t.origin_country, &f.origin_country),
    ] {
        let value = value.trim();
        if !value.is_empty() {
            parts.push(format!("{label}: {value}"));
        }
    }
    if let Some(advanced) = &query.advanced
        && !advanced.is_empty()
    {
        parts.push(fmt(t.v2_query_summary, &["Advanced"]));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::{
        StoredQuery, decode_stored_queries, decode_stored_queries_v2,
        decode_stored_queries_with_fallback, encode_stored_queries, encode_stored_queries_v2,
    };
    use crate::db::{Filters, Query};
    use crate::search::{ConditionOp, ConditionValue, FieldRef, QueryCondition, QueryExpr};

    #[test]
    fn stored_queries_round_trip_full_query() {
        let query = Query {
            text: "Widget\tQ2%2024".into(),
            filters: Filters {
                year: "2024".into(),
                product_code: "SKU-42".into(),
                sender: "A\nB".into(),
                origin_country: "CN".into(),
                ..Filters::default()
            },
            advanced: None,
            record_scope: Default::default(),
        };
        let stored = vec![StoredQuery {
            name: "Widget saved".into(),
            query: query.clone(),
        }];

        let encoded = encode_stored_queries(&stored);
        let decoded = decode_stored_queries(&encoded);

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].name, "Widget saved");
        assert_eq!(decoded[0].query, query);
    }

    #[test]
    fn stored_queries_v2_round_trip_advanced_query() {
        let query = Query {
            text: "phones".into(),
            filters: Filters::default(),
            advanced: Some(QueryExpr::Condition(QueryCondition {
                field: FieldRef::Column("sender".into()),
                op: ConditionOp::Contains,
                value: ConditionValue::Single("Widget".into()),
                negated: true,
            })),
            record_scope: Default::default(),
        };
        let stored = vec![StoredQuery {
            name: "No Widget suppliers".into(),
            query: query.clone(),
        }];

        let encoded = encode_stored_queries_v2(&stored);
        let decoded = decode_stored_queries_v2(&encoded);

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].name, "No Widget suppliers");
        assert_eq!(decoded[0].query, query);
    }

    #[test]
    fn stored_queries_fallback_reads_legacy_v1() {
        let legacy_query = Query {
            text: "legacy".into(),
            filters: Filters {
                year: "2024".into(),
                ..Filters::default()
            },
            advanced: None,
            record_scope: Default::default(),
        };
        let legacy = vec![StoredQuery {
            name: "Legacy".into(),
            query: legacy_query.clone(),
        }];

        let decoded =
            decode_stored_queries_with_fallback(None, Some(encode_stored_queries(&legacy)));

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].query, legacy_query);
    }
}
