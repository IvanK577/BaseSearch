use std::collections::{BTreeMap, HashSet};

use base_search::db::{
    Db, SourceMappingColumn, SourceMappingProfileError, SourceMappingProfileUpsert,
    source_mapping_signature,
};
use base_search::domain::table::{ColumnRole, SemanticField};
use rusqlite::{Connection, params};

fn columns() -> Vec<SourceMappingColumn> {
    vec![
        SourceMappingColumn {
            header: "Product code".to_string(),
            role: ColumnRole::Code,
        },
        SourceMappingColumn {
            header: "Net kg".to_string(),
            role: ColumnRole::Weight,
        },
        SourceMappingColumn {
            header: "Description".to_string(),
            role: ColumnRole::Text,
        },
    ]
}

fn signature() -> String {
    source_mapping_signature(&columns())
}

fn draft(name: &str, mapping: Vec<Option<SemanticField>>) -> SourceMappingProfileUpsert {
    SourceMappingProfileUpsert {
        id: None,
        name: name.to_string(),
        signature: signature(),
        mapping,
        fixed_values: BTreeMap::new(),
    }
}

#[test]
fn source_signature_normalizes_case_and_whitespace_but_preserves_structure() {
    let normalized = source_mapping_signature(&columns());
    let equivalent = source_mapping_signature(&[
        SourceMappingColumn {
            header: "  PRODUCT\t code  ".to_string(),
            role: ColumnRole::Code,
        },
        SourceMappingColumn {
            header: "net\nKG".to_string(),
            role: ColumnRole::Weight,
        },
        SourceMappingColumn {
            header: "DESCRIPTION".to_string(),
            role: ColumnRole::Text,
        },
    ]);
    assert_eq!(normalized, equivalent);

    let mut reordered = columns();
    reordered.swap(0, 1);
    assert_ne!(normalized, source_mapping_signature(&reordered));

    let mut changed_role = columns();
    changed_role[0].role = ColumnRole::Identifier;
    assert_ne!(normalized, source_mapping_signature(&changed_role));

    let fewer = &columns()[..2];
    assert_ne!(normalized, source_mapping_signature(fewer));
}

#[test]
fn profile_crud_supports_multiple_profiles_for_the_same_signature() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("profiles.db");
    let db = Db::open(&db_path).unwrap();

    let alpha = db
        .upsert_source_mapping_profile(SourceMappingProfileUpsert {
            fixed_values: BTreeMap::from([
                (SemanticField::Currency, "USD".to_string()),
                (SemanticField::WeightUnit, "KG".to_string()),
            ]),
            ..draft(
                "Alpha mapping",
                vec![
                    Some(SemanticField::ProductCode),
                    Some(SemanticField::NetWeight),
                    Some(SemanticField::Description),
                ],
            )
        })
        .unwrap();
    let beta = db
        .upsert_source_mapping_profile(draft(
            "Beta mapping",
            vec![
                Some(SemanticField::ProductCode),
                None,
                Some(SemanticField::Description),
            ],
        ))
        .unwrap();
    assert_ne!(alpha.id, beta.id);
    assert_eq!(alpha.signature, beta.signature);

    let suggestions = db.suggest_source_mapping_profiles(&signature()).unwrap();
    assert!(suggestions.ignored_corrupt_rows.is_empty());
    assert_eq!(suggestions.profiles.len(), 2);
    assert_eq!(
        suggestions
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<HashSet<_>>(),
        HashSet::from(["Alpha mapping", "Beta mapping"])
    );

    let fetched = db.get_source_mapping_profile(alpha.id).unwrap().unwrap();
    assert_eq!(fetched, alpha);

    let updated = db
        .upsert_source_mapping_profile(SourceMappingProfileUpsert {
            id: Some(alpha.id),
            name: "Alpha renamed".to_string(),
            signature: signature(),
            mapping: vec![
                Some(SemanticField::ProductCode),
                Some(SemanticField::GrossWeight),
                Some(SemanticField::Description),
            ],
            fixed_values: BTreeMap::from([
                (SemanticField::Currency, "EUR".to_string()),
                (SemanticField::WeightUnit, "KG".to_string()),
            ]),
        })
        .unwrap();
    assert_eq!(updated.id, alpha.id);
    assert_eq!(updated.name, "Alpha renamed");
    assert_eq!(updated.created_at, alpha.created_at);

    let upserted_by_name = db
        .upsert_source_mapping_profile(SourceMappingProfileUpsert {
            id: None,
            name: "  ALPHA RENAMED  ".to_string(),
            signature: signature(),
            mapping: vec![None, Some(SemanticField::NetWeight), None],
            fixed_values: BTreeMap::from([(SemanticField::Currency, "USD".to_string())]),
        })
        .unwrap();
    assert_eq!(upserted_by_name.id, alpha.id);
    assert_eq!(
        upserted_by_name.fixed_values,
        BTreeMap::from([(SemanticField::Currency, "USD".to_string())])
    );
    assert_eq!(db.list_source_mapping_profiles().unwrap().profiles.len(), 2);

    assert!(db.delete_source_mapping_profile(beta.id).unwrap());
    assert!(!db.delete_source_mapping_profile(beta.id).unwrap());
    assert!(db.get_source_mapping_profile(beta.id).unwrap().is_none());
}

#[test]
fn profiles_persist_across_reopen_and_validate_public_input() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("persistent.db");
    let saved = {
        let db = Db::open(&db_path).unwrap();
        db.upsert_source_mapping_profile(SourceMappingProfileUpsert {
            fixed_values: BTreeMap::from([
                (SemanticField::Currency, "UAH".to_string()),
                (SemanticField::WeightUnit, "KG".to_string()),
            ]),
            ..draft(
                "Persistent",
                vec![
                    Some(SemanticField::ProductCode),
                    Some(SemanticField::NetWeight),
                    None,
                ],
            )
        })
        .unwrap()
    };

    let reopened = Db::open(&db_path).unwrap();
    assert_eq!(
        reopened
            .get_source_mapping_profile(saved.id)
            .unwrap()
            .unwrap(),
        saved
    );

    for invalid in [
        SourceMappingProfileUpsert {
            id: None,
            name: "   ".to_string(),
            signature: signature(),
            mapping: vec![None; 3],
            fixed_values: BTreeMap::new(),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "x".repeat(101),
            signature: signature(),
            mapping: vec![None; 3],
            fixed_values: BTreeMap::new(),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "Too many columns".to_string(),
            signature: source_mapping_signature(
                &(0..4097)
                    .map(|index| SourceMappingColumn {
                        header: format!("Column {index}"),
                        role: ColumnRole::Text,
                    })
                    .collect::<Vec<_>>(),
            ),
            mapping: vec![None; 4097],
            fixed_values: BTreeMap::new(),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "Mismatched mapping".to_string(),
            signature: signature(),
            mapping: vec![None; 2],
            fixed_values: BTreeMap::new(),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "Unsafe fixed semantic".to_string(),
            signature: signature(),
            mapping: vec![None; 3],
            fixed_values: BTreeMap::from([(SemanticField::ProductCode, "1234".to_string())]),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "Empty fixed value".to_string(),
            signature: signature(),
            mapping: vec![None; 3],
            fixed_values: BTreeMap::from([(SemanticField::Currency, "   ".to_string())]),
        },
        SourceMappingProfileUpsert {
            id: None,
            name: "Long fixed value".to_string(),
            signature: signature(),
            mapping: vec![None; 3],
            fixed_values: BTreeMap::from([(SemanticField::WeightUnit, "x".repeat(33))]),
        },
    ] {
        assert!(matches!(
            reopened.upsert_source_mapping_profile(invalid),
            Err(SourceMappingProfileError::Validation(_))
        ));
    }
}

#[test]
fn list_and_suggest_report_corrupt_rows_while_get_returns_a_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corrupt.db");
    let valid_id = {
        let db = Db::open(&db_path).unwrap();
        db.upsert_source_mapping_profile(draft(
            "Valid",
            vec![Some(SemanticField::ProductCode), None, None],
        ))
        .unwrap()
        .id
    };

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO source_mapping_profiles (
            name, name_key, signature, mapping_json, fixed_values_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            "Corrupt",
            "corrupt",
            signature(),
            r#"["UnknownSemantic",null,null]"#,
            r#"{}"#,
            "2026-01-01T00:00:00.000Z"
        ],
    )
    .unwrap();
    let corrupt_id = conn.last_insert_rowid();
    drop(conn);

    let db = Db::open(&db_path).unwrap();
    let listed = db.list_source_mapping_profiles().unwrap();
    assert_eq!(listed.profiles.len(), 1);
    assert_eq!(listed.profiles[0].id, valid_id);
    assert_eq!(listed.ignored_corrupt_rows.len(), 1);
    assert_eq!(listed.ignored_corrupt_rows[0].id, corrupt_id);

    let suggested = db.suggest_source_mapping_profiles(&signature()).unwrap();
    assert_eq!(suggested.profiles.len(), 1);
    assert_eq!(suggested.ignored_corrupt_rows.len(), 1);

    assert!(matches!(
        db.get_source_mapping_profile(corrupt_id),
        Err(SourceMappingProfileError::CorruptRow { id, .. }) if id == corrupt_id
    ));
}

#[test]
fn opening_a_database_without_profile_schema_is_additive() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v1.db");
    {
        let db = Db::open(&db_path).unwrap();
        db.diagnostic_execute(
            "INSERT INTO records(row_hash, source_file) VALUES (zeroblob(16), 'v1.xlsx')",
        )
        .unwrap();
        db.diagnostic_execute(
            "INSERT INTO records_fts(rowid, search_text) VALUES (1, 'legacy marker')",
        )
        .unwrap();
    }

    let conn = Connection::open(&db_path).unwrap();
    let records_rootpage: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'records'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute_batch(
        "DROP TABLE source_mapping_profiles;
         CREATE TRIGGER v1_records_no_update
         BEFORE UPDATE ON records BEGIN
             SELECT RAISE(FAIL, 'records table was rewritten');
         END;",
    )
    .unwrap();
    drop(conn);

    let db = Db::open(&db_path).unwrap();
    assert!(
        db.list_source_mapping_profiles()
            .unwrap()
            .profiles
            .is_empty()
    );
    assert_eq!(db.total_rows(), 1);
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT rowid FROM records_fts WHERE records_fts MATCH 'legacy'",
            1,
        )
        .unwrap()[0][0],
        "1"
    );
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'records'",
            1,
        )
        .unwrap()[0][0],
        records_rootpage.to_string()
    );
}
