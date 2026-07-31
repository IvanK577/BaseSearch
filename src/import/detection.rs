use std::collections::{BTreeMap, HashMap, HashSet};

use calamine::Data;

use crate::domain::table::SemanticField;
use crate::schema::COLUMNS;

use super::aliases::{AliasMatch, canonical_column, match_header, normalize_header};
use super::{ColSrc, HEADER_SCAN_ROWS, cell_has_value, header_text, normalize_value};

const SAMPLE_ROWS: usize = 8;
/// Rows inspected when judging what a column holds. The detection buffer
/// already contains them, so a wider window costs nothing and stops a handful
/// of blank leading cells from deciding a column's fate.
const VALUE_SAMPLE_ROWS: usize = DETECTION_BUFFER_ROWS;
pub(super) const DETECTION_BUFFER_ROWS: usize = HEADER_SCAN_ROWS + SAMPLE_ROWS;

pub(super) struct MappingPlan {
    pub columns: Vec<ColSrc>,
    pub semantics: BTreeMap<usize, SemanticField>,
}

pub(super) struct DetectedTable {
    pub header_index: usize,
    pub headers: Vec<String>,
    pub plan: MappingPlan,
}

impl DetectedTable {
    pub fn layout(&self) -> &'static str {
        "generic table"
    }
}

pub(super) fn detect_table(scanned: &[Vec<Data>]) -> Option<DetectedTable> {
    let candidate_rows = scanned.len().min(HEADER_SCAN_ROWS);
    let header_index = (0..candidate_rows)
        .filter_map(|index| header_score(scanned, index).map(|score| (index, score)))
        .max_by(|(left_index, left_score), (right_index, right_score)| {
            left_score
                .cmp(right_score)
                .then_with(|| right_index.cmp(left_index))
        })?
        .0;
    let width = table_width(scanned, header_index);
    let raw_headers = raw_headers(&scanned[header_index], width);
    let headers = display_headers(&raw_headers);
    let samples = &scanned[(header_index + 1)..];
    let semantics = infer_semantics(&raw_headers, samples);
    let columns = canonical_mapping(&raw_headers, &semantics, samples);
    Some(DetectedTable {
        header_index,
        headers,
        plan: MappingPlan { columns, semantics },
    })
}

fn table_width(scanned: &[Vec<Data>], header_index: usize) -> usize {
    scanned
        .iter()
        .skip(header_index)
        .take(SAMPLE_ROWS + 1)
        .map(|row| {
            row.iter()
                .rposition(cell_has_value)
                .map(|index| index + 1)
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn raw_headers(row: &[Data], width: usize) -> Vec<String> {
    (0..width)
        .map(|index| row.get(index).map(header_text).unwrap_or_default())
        .collect()
}

fn display_headers(headers: &[String]) -> Vec<String> {
    let mut seen = HashMap::<String, usize>::new();
    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            let base = if header.trim().is_empty() {
                format!("Column {}", index + 1)
            } else {
                header.clone()
            };
            let key = normalize_header(&base);
            let count = seen.entry(key).or_default();
            *count += 1;
            if *count == 1 {
                base
            } else {
                format!("{base} ({count})")
            }
        })
        .collect()
}

fn header_score(scanned: &[Vec<Data>], index: usize) -> Option<i64> {
    let row = &scanned[index];
    let non_empty = row.iter().filter(|cell| cell_has_value(cell)).count();
    if non_empty == 0 {
        return None;
    }

    let labels = row
        .iter()
        .filter(|cell| looks_like_header_label(cell))
        .count();
    let numeric = row.iter().filter(|cell| looks_numeric_cell(cell)).count();
    let aliases = row
        .iter()
        .filter(|cell| match_header(&header_text(cell)).is_some())
        .count();
    let unique = row
        .iter()
        .map(header_text)
        .map(|header| normalize_header(&header))
        .filter(|header| !header.is_empty())
        .collect::<HashSet<_>>()
        .len();
    let width = row
        .iter()
        .rposition(cell_has_value)
        .map(|position| position + 1)
        .unwrap_or(0);
    let consistent_following = scanned
        .iter()
        .skip(index + 1)
        .take(SAMPLE_ROWS)
        .filter(|candidate| {
            let candidate_width = candidate
                .iter()
                .rposition(cell_has_value)
                .map(|position| position + 1)
                .unwrap_or(0);
            candidate_width > 0 && candidate_width.abs_diff(width) <= (width / 5).max(1)
        })
        .count();
    let transitions = (0..width)
        .filter(|column| {
            row.get(*column).is_some_and(looks_like_header_label)
                && scanned
                    .iter()
                    .skip(index + 1)
                    .take(SAMPLE_ROWS)
                    .filter_map(|sample| sample.get(*column))
                    .any(|sample| !looks_like_header_label(sample))
        })
        .count();

    Some(
        non_empty as i64 * 20
            + labels as i64 * 6
            + aliases as i64 * 12
            + unique as i64 * 2
            + consistent_following as i64 * 3
            + transitions as i64 * 2
            - numeric as i64 * 10,
    )
}

fn looks_like_header_label(cell: &Data) -> bool {
    let text = header_text(cell);
    if text.is_empty() || text.chars().count() > 120 {
        return false;
    }
    let letters = text
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    let digits = text
        .chars()
        .filter(|character| character.is_ascii_digit())
        .count();
    letters > 0 && letters >= digits
}

fn looks_numeric_cell(cell: &Data) -> bool {
    match cell {
        Data::Float(_) | Data::Int(_) | Data::DateTime(_) => true,
        Data::String(value) => parse_bare_number(value).is_some(),
        _ => false,
    }
}

fn infer_semantics(headers: &[String], samples: &[Vec<Data>]) -> BTreeMap<usize, SemanticField> {
    let mut inferred = BTreeMap::new();
    for (index, header) in headers.iter().enumerate() {
        let Some(alias) = match_header(header) else {
            continue;
        };
        if alias.needs_sample_confirmation && !samples_confirm(alias, samples, index) {
            continue;
        }
        inferred.insert(index, alias.semantic);
    }

    resolve_participant_duplicates(&mut inferred, samples);
    resolve_other_duplicates(&mut inferred, samples);
    inferred
}

fn samples_confirm(alias: AliasMatch, samples: &[Vec<Data>], column: usize) -> bool {
    let values = sample_values(samples, column);
    if values.is_empty() {
        return false;
    }
    let matches = values
        .iter()
        .filter(|value| match alias.semantic {
            // Must agree with the num_value SQL function the search and
            // analytics use, or a column is rejected at import and then
            // parses perfectly at query time. The local parser turns every
            // comma into a dot, so "1.234,56" and "1200.75 USD" failed here
            // while the runtime read them fine.
            SemanticField::Value | SemanticField::Quantity => {
                crate::storage::normalize::parse_number(value).is_some()
            }
            SemanticField::Country => looks_like_country(value),
            _ => true,
        })
        .count();
    matches * 10 >= values.len() * 7
}

fn resolve_participant_duplicates(
    inferred: &mut BTreeMap<usize, SemanticField>,
    samples: &[Vec<Data>],
) {
    let has_explicit_company_code = inferred
        .values()
        .any(|semantic| *semantic == SemanticField::CompanyCode);
    let mut candidates = Vec::new();

    for participant in [SemanticField::Recipient, SemanticField::Sender] {
        let columns = inferred
            .iter()
            .filter_map(|(index, semantic)| (*semantic == participant).then_some(*index))
            .collect::<Vec<_>>();
        if columns.len() <= 1 {
            continue;
        }

        let code_candidates = columns
            .iter()
            .copied()
            .filter(|column| sample_code_score(samples, *column) >= 0.8)
            .collect::<Vec<_>>();
        let name_candidates = columns
            .iter()
            .copied()
            .filter(|column| sample_name_score(samples, *column) >= 0.7)
            .collect::<Vec<_>>();

        for column in &columns {
            inferred.remove(column);
        }
        // Ambiguity must never cost the column outright. Dropping the
        // participant silently empties the "Recipients / importers" section and
        // leaves the company dossier with no name, while the EDRPOU section
        // keeps working — which reads to the user as "the importer is gone and
        // only its code is left". Columns are in file order, so falling back to
        // the leftmost match keeps the primary name column, which registries
        // place before its synonyms.
        let chosen = match name_candidates.as_slice() {
            // Unambiguous: exactly one column looks like a name.
            [only] => Some(*only),
            // Several name-like synonyms, e.g. "Отримувач" and "Покупець".
            [first, ..] => Some(*first),
            // None look like names: prefer any column that is not a bare code.
            [] => columns
                .iter()
                .copied()
                .find(|column| !code_candidates.contains(column))
                .or_else(|| columns.first().copied()),
        };
        if let Some(chosen) = chosen {
            inferred.insert(chosen, participant);
        }
        // The implicit company-code inference stays strict: a code is only
        // adopted from an unambiguous name/code pair, never from a file whose
        // participant columns were guessed.
        if code_candidates.len() == 1 && name_candidates.len() == 1 {
            candidates.push(code_candidates[0]);
        }
    }

    if !has_explicit_company_code && candidates.len() == 1 {
        inferred.insert(candidates[0], SemanticField::CompanyCode);
    }
}

fn resolve_other_duplicates(inferred: &mut BTreeMap<usize, SemanticField>, samples: &[Vec<Data>]) {
    let mut grouped = HashMap::<SemanticField, Vec<usize>>::new();
    for (index, semantic) in inferred.iter() {
        grouped.entry(*semantic).or_default().push(*index);
    }

    for (semantic, columns) in grouped {
        if columns.len() <= 1
            || semantic == SemanticField::DeclarationNumber
            || matches!(
                semantic,
                SemanticField::Recipient | SemanticField::Sender | SemanticField::CompanyCode
            )
        {
            continue;
        }
        let populated = columns
            .iter()
            .map(|column| (*column, sample_values(samples, *column).len()))
            .collect::<Vec<_>>();
        let max = populated.iter().map(|(_, count)| *count).max().unwrap_or(0);
        let winners = populated
            .iter()
            .filter_map(|(column, count)| (*count == max && max > 0).then_some(*column))
            .collect::<Vec<_>>();
        for column in &columns {
            inferred.remove(column);
        }
        if winners.len() == 1 {
            inferred.insert(winners[0], semantic);
        }
    }
}

fn canonical_mapping(
    headers: &[String],
    semantics: &BTreeMap<usize, SemanticField>,
    samples: &[Vec<Data>],
) -> Vec<ColSrc> {
    let mut mapping = vec![ColSrc::Missing; COLUMNS.len()];
    let mut direct = HashMap::<usize, Vec<usize>>::new();
    for (source_index, header) in headers.iter().enumerate() {
        if let Some(target_index) = canonical_column(header) {
            direct.entry(target_index).or_default().push(source_index);
        }
    }
    for (target_index, mut sources) in direct {
        sources.sort_unstable();
        mapping[target_index] = choose_canonical_source(target_index, sources, samples);
    }

    let mut by_target = HashMap::<usize, Vec<usize>>::new();
    for (source_index, semantic) in semantics {
        let Some(target_name) = crate::schema::column_for_semantic(*semantic) else {
            continue;
        };
        let Some(target_index) = COLUMNS.iter().position(|column| column.name == target_name)
        else {
            continue;
        };
        by_target
            .entry(target_index)
            .or_default()
            .push(*source_index);
    }

    for (target_index, mut sources) in by_target {
        if !matches!(mapping[target_index], ColSrc::Missing) {
            continue;
        }
        sources.sort_unstable();
        mapping[target_index] = if sources.len() == 1 {
            ColSrc::Cell(sources[0])
        } else {
            ColSrc::Join(sources, "/")
        };
    }
    mapping
}

fn choose_canonical_source(
    target_index: usize,
    sources: Vec<usize>,
    samples: &[Vec<Data>],
) -> ColSrc {
    if sources.len() == 1 {
        return ColSrc::Cell(sources[0]);
    }
    if matches!(
        COLUMNS[target_index].name,
        "declaration_number" | "declaration_type"
    ) {
        return ColSrc::Join(sources, "/");
    }

    let populated = sources
        .iter()
        .map(|source| (*source, sample_values(samples, *source).len()))
        .collect::<Vec<_>>();
    let max = populated.iter().map(|(_, count)| *count).max().unwrap_or(0);
    let winners = populated
        .iter()
        .filter_map(|(source, count)| (*count == max && max > 0).then_some(*source))
        .collect::<Vec<_>>();
    if winners.len() == 1 {
        ColSrc::Cell(winners[0])
    } else {
        ColSrc::Missing
    }
}

fn sample_values(samples: &[Vec<Data>], column: usize) -> Vec<String> {
    samples
        .iter()
        .take(VALUE_SAMPLE_ROWS)
        .filter_map(|row| row.get(column))
        .map(normalize_value)
        .filter(|value| !value.is_empty())
        .collect()
}

fn sample_code_score(samples: &[Vec<Data>], column: usize) -> f64 {
    let values = sample_values(samples, column);
    if values.is_empty() {
        return 0.0;
    }
    let matches = values
        .iter()
        .filter(|value| {
            let compact = value
                .chars()
                .filter(|character| character.is_alphanumeric())
                .collect::<String>();
            let digits = compact
                .chars()
                .filter(|character| character.is_ascii_digit())
                .count();
            (4..=24).contains(&compact.chars().count())
                && !value.chars().any(char::is_whitespace)
                && digits * 2 >= compact.chars().count()
        })
        .count();
    matches as f64 / values.len() as f64
}

fn sample_name_score(samples: &[Vec<Data>], column: usize) -> f64 {
    let values = sample_values(samples, column);
    if values.is_empty() {
        return 0.0;
    }
    let matches = values
        .iter()
        .filter(|value| {
            let letters = value
                .chars()
                .filter(|character| character.is_alphabetic())
                .count();
            let digits = value
                .chars()
                .filter(|character| character.is_ascii_digit())
                .count();
            letters >= 3 && letters > digits
        })
        .count();
    matches as f64 / values.len() as f64
}

fn looks_like_country(value: &str) -> bool {
    let trimmed = value.trim();
    let characters = trimmed.chars().count();
    (2..=64).contains(&characters)
        && trimmed.chars().all(|character| {
            character.is_alphabetic() || character.is_whitespace() || character == '-'
        })
}

/// Strict "this cell is a bare number" test, used when scoring whether a row
/// looks like a header. Tolerating units here would make a data row look
/// numeric in the wrong places, so it stays deliberately narrow.
fn parse_bare_number(value: &str) -> Option<f64> {
    let compact = value
        .trim()
        .replace([' ', '\u{00a0}', '\u{202f}'], "")
        .replace(',', ".");
    compact
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

#[cfg(test)]
mod tests {
    use calamine::Data;

    use super::detect_table;
    use crate::domain::table::SemanticField;

    fn row(values: &[&str]) -> Vec<Data> {
        values
            .iter()
            .map(|value| Data::String((*value).to_string()))
            .collect()
    }

    #[test]
    fn title_rows_do_not_beat_a_wider_unknown_header() {
        let rows = vec![
            row(&["Inventory export"]),
            row(&["Nebula", "Flux", "Comment"]),
            row(&["N-01", "12.5", "Stable"]),
        ];
        let detected = detect_table(&rows).unwrap();
        assert_eq!(detected.header_index, 1);
        assert!(detected.plan.semantics.is_empty());
    }

    #[test]
    fn repeated_participant_headers_use_sample_shape() {
        let rows = vec![
            row(&["Одержувач", "Одержувач"]),
            row(&["37642136", "ТОВ Приклад"]),
            row(&["32818783", "ТОВ Інший"]),
        ];
        let detected = detect_table(&rows).unwrap();
        assert_eq!(
            detected.plan.semantics.get(&0),
            Some(&SemanticField::CompanyCode)
        );
        assert_eq!(
            detected.plan.semantics.get(&1),
            Some(&SemanticField::Recipient)
        );
    }
}
