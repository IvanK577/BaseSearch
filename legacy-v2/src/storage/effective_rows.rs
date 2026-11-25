//! SQL contract for compact duplicate occurrences.
//!
//! `r` is always the physical occurrence row. It owns identity and provenance.
//! `p` is the payload row: the referenced canonical row for compact duplicates,
//! or the occurrence itself for canonical and legacy full rows.

pub(crate) const OCCURRENCE_ALIAS: &str = "r";
pub(crate) const PAYLOAD_ALIAS: &str = "p";

pub(crate) fn payload_join() -> &'static str {
    " JOIN records p ON p.id = COALESCE(
         r.canonical_id,
         CASE
             WHEN r.dup_first_file IS NULL THEN r.id
             ELSE COALESCE(
                 (
                     SELECT legacy_owner.id
                     FROM records legacy_owner
                     WHERE legacy_owner.row_hash = r.row_hash
                       AND legacy_owner.schema_id IS r.schema_id
                       AND legacy_owner.dup_first_file IS NULL
                       AND legacy_owner.canonical_id IS NULL
                     ORDER BY legacy_owner.id
                     LIMIT 1
                 ),
                 r.id
             )
         END
     ) AND p.schema_id IS r.schema_id"
}

pub(crate) fn payload_column(payload_alias: &str, name: &str) -> String {
    format!("{payload_alias}.{name}")
}

pub(crate) fn result_column(payload_alias: &str, name: &str) -> String {
    if is_occurrence_owned(name) {
        format!("{OCCURRENCE_ALIAS}.{name}")
    } else {
        payload_column(payload_alias, name)
    }
}

pub(crate) fn is_occurrence_owned(name: &str) -> bool {
    matches!(
        name,
        "id" | "row_hash"
            | "source_file"
            | "dup_first_file"
            | "canonical_id"
            | "schema_id"
            | "source_id"
            | "imported_at"
    )
}

pub(crate) fn canonical_scope_clause() -> &'static str {
    // Both legacy and compact duplicate occurrences carry dup_first_file.
    // Keeping this predicate stable preserves the existing scoped indexes.
    "r.dup_first_file IS NULL"
}

pub(crate) fn searchable_payload_clause(alias: &str) -> String {
    format!(
        "{alias}.canonical_id IS NULL AND (
             {alias}.dup_first_file IS NULL OR NOT EXISTS (
                 SELECT 1
                 FROM records searchable_owner
                 WHERE searchable_owner.row_hash = {alias}.row_hash
                   AND searchable_owner.schema_id IS {alias}.schema_id
                   AND searchable_owner.dup_first_file IS NULL
                   AND searchable_owner.canonical_id IS NULL
             )
         )"
    )
}

#[cfg(test)]
mod tests {
    use super::{canonical_scope_clause, payload_join, result_column};

    #[test]
    fn payload_and_provenance_have_distinct_owners() {
        assert!(payload_join().contains("legacy_owner.row_hash = r.row_hash"));
        assert_eq!(result_column("p", "description"), "p.description");
        assert_eq!(result_column("p", "source_file"), "r.source_file");
        assert_eq!(canonical_scope_clause(), "r.dup_first_file IS NULL");
    }
}
