use rusqlite::Connection;

use crate::domain::table::{SourceColumn, TableShape};
use crate::storage::meta;

pub(crate) const TABLE_SHAPE_KEY: &str = "table_shape_v1";

pub(crate) fn get(conn: &Connection) -> Option<TableShape> {
    meta::get(conn, TABLE_SHAPE_KEY).and_then(|raw| serde_json::from_str::<TableShape>(&raw).ok())
}

/// The shape the rest of the app should see: the compatibility shape derived
/// from the registered source schemas when any exist, else the legacy metadata
/// blob written by older imports. Analytics, query planning, and catalogs all
/// resolve columns through this one view.
pub(crate) fn effective(conn: &Connection) -> Option<TableShape> {
    crate::storage::source_schemas::compatibility_shape(conn)
        .ok()
        .flatten()
        .or_else(|| get(conn))
}

pub(crate) fn set(conn: &Connection, shape: &TableShape) {
    if let Ok(raw) = serde_json::to_string(shape) {
        meta::set(conn, TABLE_SHAPE_KEY, &raw);
    }
}

pub(crate) fn merge(conn: &Connection, incoming: &TableShape) -> TableShape {
    let mut merged = get(conn).unwrap_or_else(|| TableShape {
        columns: Vec::new(),
    });
    for column in &incoming.columns {
        merge_column(&mut merged.columns, column);
    }
    set(conn, &merged);
    merged
}

/// Folds one column into the merged set and returns the id it landed on:
/// the existing id when the column unifies with a known one, or a suffixed
/// fresh id when the same id arrives with a different header/storage.
pub(crate) fn merge_column(columns: &mut Vec<SourceColumn>, incoming: &SourceColumn) -> String {
    if let Some(existing) = columns.iter_mut().find(|column| column.id == incoming.id) {
        if existing.semantic.is_none() {
            existing.semantic = incoming.semantic;
        }
        if existing.storage == incoming.storage && existing.header == incoming.header {
            return incoming.id.clone();
        }
        let mut additional = incoming.clone();
        additional.id = unique_column_id(columns, &incoming.id);
        let assigned = additional.id.clone();
        columns.push(additional);
        return assigned;
    }
    columns.push(incoming.clone());
    incoming.id.clone()
}

fn unique_column_id(columns: &[SourceColumn], base: &str) -> String {
    let mut index = 2usize;
    loop {
        let candidate = format!("{base}_{index}");
        if !columns.iter().any(|column| column.id == candidate) {
            return candidate;
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{get, merge};
    use crate::domain::table::TableShape;

    #[test]
    fn shape_metadata_merges_columns_without_dropping_source_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();

        merge(
            &conn,
            &TableShape::from_headers(["SKU".to_string(), "Price".to_string()]),
        );
        merge(
            &conn,
            &TableShape::from_headers(["SKU".to_string(), "Warehouse".to_string()]),
        );

        let shape = get(&conn).unwrap();
        let ids = shape
            .columns
            .iter()
            .map(|column| column.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["sku", "price", "warehouse"]);
    }

    #[test]
    fn shape_metadata_keeps_extra_and_schema_sources_for_same_semantic() {
        use crate::domain::table::{ColumnStorage, SemanticField};

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();

        merge(
            &conn,
            &TableShape::from_headers(["Product code".to_string()]),
        );
        let mut schema_shape = TableShape::from_headers(["Product code".to_string()]);
        schema_shape.columns[0].semantic = Some(SemanticField::ProductCode);
        schema_shape.columns[0].storage = ColumnStorage::SchemaColumn("product_code".to_string());
        merge(&conn, &schema_shape);

        let shape = get(&conn).unwrap();
        let product_sources = shape
            .columns
            .iter()
            .filter(|column| column.semantic == Some(SemanticField::ProductCode))
            .collect::<Vec<_>>();
        assert_eq!(product_sources.len(), 2);
        assert!(
            product_sources
                .iter()
                .any(|column| column.storage == ColumnStorage::SourceJson)
        );
        assert!(
            product_sources
                .iter()
                .any(|column| matches!(column.storage, ColumnStorage::SchemaColumn(_)))
        );
    }
}
