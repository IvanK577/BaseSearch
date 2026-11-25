use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const EXTRA_HEADERS_KEY: &str = "extra_headers_v1";

pub(crate) fn get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

pub(crate) fn set(conn: &Connection, key: &str, value: &str) {
    let _ = conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    );
}

pub(crate) fn get_i64(conn: &Connection, key: &str) -> i64 {
    get(conn, key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

pub(crate) fn get_string_vec(conn: &Connection, key: &str) -> Option<Vec<String>> {
    let raw = get(conn, key)?;
    serde_json::from_str::<Vec<String>>(&raw).ok()
}

pub(crate) fn set_string_vec(conn: &Connection, key: &str, values: &[String]) {
    if let Ok(json) = serde_json::to_string(values) {
        set(conn, key, &json);
    }
}

pub(crate) fn delete(conn: &Connection, key: &str) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key])
}

#[cfg(test)]
mod tests {
    use super::{get, get_i64, get_string_vec, set, set_string_vec};
    use rusqlite::Connection;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn meta_round_trips_strings_and_numbers() {
        let conn = memory_conn();

        set(&conn, "lang", "en");
        set(&conn, "count", "42");

        assert_eq!(get(&conn, "lang").as_deref(), Some("en"));
        assert_eq!(get_i64(&conn, "count"), 42);
        assert_eq!(get_i64(&conn, "missing"), 0);
    }

    #[test]
    fn meta_round_trips_string_vectors() {
        let conn = memory_conn();
        let values = vec!["Container".to_string(), "Invoice".to_string()];

        set_string_vec(&conn, "headers", &values);

        assert_eq!(get_string_vec(&conn, "headers"), Some(values));
    }
}
