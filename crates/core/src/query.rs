use std::collections::BTreeMap;

use rusqlite::{Connection, Row, Transaction, params, params_from_iter};

use crate::model::{
    Column, FilterOp, QueryFilter, QuerySpec, Record, Role, SearchPage, SortDirection,
};
use crate::workspace::{
    Workspace, display_decimal, fold_text, metadata, normalize_currency, normalize_date,
    normalize_weight_unit, parse_decimal, read_count,
};
use crate::{Error, Result};

const PAGE_LIMIT: u64 = 500;
pub(crate) const RECORD_COLUMNS: &str = "r.id, s.file_name, s.sheet, r.row_number, r.raw";

pub fn search(
    workspace: &Workspace,
    query: &QuerySpec,
    offset: u64,
    limit: u64,
) -> Result<SearchPage> {
    let mut conn = workspace.connect()?;
    let tx = conn.transaction()?;
    let (revision, rows) = snapshot(&tx, query.revision)?;
    let plan = Plan::new(&tx, query)?;
    let total_rows = plan.count(&tx, rows)?;
    let limit = limit.min(PAGE_LIMIT);
    let mut rows = Vec::with_capacity(limit as usize);
    if offset < total_rows && limit > 0 {
        let (sql, mut values) = plan.ordered(RecordProjection::WithSource);
        let mut statement = tx.prepare(&format!("{sql} LIMIT ? OFFSET ?"))?;
        values.push((limit as i64).into());
        values.push((offset as i64).into());
        let mut cursor = statement.query(params_from_iter(&values))?;
        while let Some(row) = cursor.next()? {
            rows.push(decode_record(row)?);
        }
    }
    Ok(SearchPage {
        revision,
        total_rows,
        rows,
    })
}

pub fn record(workspace: &Workspace, id: i64, revision: Option<i64>) -> Result<Option<Record>> {
    let mut conn = workspace.connect()?;
    let tx = conn.transaction()?;
    snapshot(&tx, revision)?;
    let mut statement = tx.prepare(&format!(
        "SELECT {RECORD_COLUMNS} FROM records AS r
         JOIN sources AS s ON s.id = r.source_id WHERE r.id = ?1"
    ))?;
    let mut rows = statement.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut record = decode_record(row)?;
    let mut headers = tx.prepare(
        "SELECT column_id, label, role FROM source_columns
         WHERE source_id = (SELECT source_id FROM records WHERE id = ?1)
         ORDER BY ordinal",
    )?;
    record.source_columns = headers
        .query_map(params![id], |row| {
            Ok(Column {
                id: row.get(0)?,
                label: row.get(1)?,
                role: row
                    .get::<_, Option<String>>(2)?
                    .as_deref()
                    .and_then(Role::from_name),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(record))
}

pub(crate) fn decode_record(row: &Row<'_>) -> Result<Record> {
    Ok(Record {
        id: row.get(0)?,
        source_name: row.get(1)?,
        sheet: row.get(2)?,
        row_number: row
            .get::<_, Option<i64>>(3)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| {
                Error::Message("В хранилище обнаружен недопустимый номер строки.".into())
            })?,
        values: decode_values(row, 4)?,
        source_columns: Vec::new(),
    })
}

pub(crate) fn decode_values(
    row: &Row<'_>,
    column: usize,
) -> Result<BTreeMap<String, Option<String>>> {
    let rusqlite::types::ValueRef::Text(encoded) = row.get_ref(column)? else {
        return Err(Error::Message("Поля строки хранилища повреждены.".into()));
    };
    Ok(serde_json::from_slice(encoded)?)
}

pub(crate) fn snapshot(tx: &Transaction<'_>, expected: Option<i64>) -> Result<(i64, u64)> {
    let (actual, rows) = metadata(tx)?;
    if let Some(expected) = expected
        && expected != actual
    {
        return Err(Error::StaleRevision { expected, actual });
    }
    Ok((actual, rows))
}

pub(crate) struct Plan {
    pub(crate) condition: String,
    pub(crate) args: Vec<rusqlite::types::Value>,
    order: String,
    sort_arg: Option<rusqlite::types::Value>,
}

pub(crate) enum RecordProjection {
    WithSource,
    Raw,
}

impl Plan {
    pub(crate) fn new(conn: &Connection, query: &QuerySpec) -> Result<Self> {
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if !query.text.trim().is_empty() {
            let terms = search_terms(&query.text);
            if terms.is_empty() {
                clauses.push("0".into());
            } else {
                clauses.push(
                    "r.id IN (SELECT rowid FROM record_text WHERE record_text MATCH ?)".into(),
                );
                args.push(terms.into());
            }
        }
        for filter in &query.filters {
            clauses.push(filter_sql(conn, filter, &mut args)?);
        }
        let condition = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let (order, sort_arg) = if let Some(sort) = &query.sort {
            let field = Field::resolve(conn, &sort.field)?;
            let mut sort_args = Vec::new();
            let target = field.sql(&mut sort_args);
            let direction = match sort.direction {
                SortDirection::Asc => "ASC",
                SortDirection::Desc => "DESC",
            };
            (
                format!("{target} {direction} NULLS LAST, r.id ASC"),
                sort_args.pop(),
            )
        } else {
            ("r.id ASC".into(), None)
        };
        Ok(Self {
            condition,
            args,
            order,
            sort_arg,
        })
    }

    pub(crate) fn count(&self, conn: &Connection, unfiltered_rows: u64) -> Result<u64> {
        if self.condition.is_empty() {
            return Ok(unfiltered_rows);
        }
        Ok(conn.query_row(
            &format!("SELECT COUNT(*) FROM records AS r{}", self.condition),
            params_from_iter(&self.args),
            |row| read_count(row, 0),
        )?)
    }

    pub(crate) fn ordered(
        &self,
        projection: RecordProjection,
    ) -> (String, Vec<rusqlite::types::Value>) {
        let source = match projection {
            RecordProjection::WithSource => format!(
                "{RECORD_COLUMNS} FROM records AS r JOIN sources AS s ON s.id = r.source_id"
            ),
            RecordProjection::Raw => "r.raw FROM records AS r".into(),
        };
        let mut args = self.args.clone();
        args.extend(self.sort_arg.clone());
        (
            format!("SELECT {source}{} ORDER BY {}", self.condition, self.order),
            args,
        )
    }
}

enum Field<'a> {
    Raw(&'a str),
    Normalized(Role),
}

impl<'a> Field<'a> {
    fn resolve(conn: &Connection, field: &'a str) -> Result<Self> {
        if let Some(role) = field.strip_prefix('@').and_then(Role::from_name) {
            return Ok(Self::Normalized(role));
        }
        let found: bool = conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM columns WHERE id = ?1)")?
            .query_row(params![field], |row| row.get(0))?;
        if found {
            return Ok(Self::Raw(field));
        }
        Err(Error::Message(format!("Неизвестное поле: {field}")))
    }

    fn sql(&self, args: &mut Vec<rusqlite::types::Value>) -> &'static str {
        match self {
            Self::Raw(id) => {
                args.push(format!("$.\"{id}\"").into());
                "(json_extract(r.raw, ?) COLLATE base_text)"
            }
            Self::Normalized(role) => match role {
                Role::Date => "r.date",
                Role::Company => "r.company",
                Role::Product => "r.product",
                Role::Value => "r.amount",
                Role::Currency => "r.currency",
                Role::Weight => "r.weight_kg",
                Role::WeightUnit => "r.weight_unit",
            },
        }
    }

    fn comparison_value(&self, value: &str) -> Result<String> {
        let value = match self {
            Self::Normalized(Role::Date) => normalize_date(value),
            Self::Normalized(Role::Value | Role::Weight) => {
                parse_decimal(value).map(display_decimal)
            }
            Self::Normalized(Role::Currency) => normalize_currency(value),
            Self::Normalized(Role::WeightUnit) => normalize_weight_unit(value).map(str::to_owned),
            _ => Some(value.to_owned()),
        };
        value.ok_or_else(|| Error::Message("Значение фильтра не соответствует типу поля.".into()))
    }
}

fn filter_sql(
    conn: &Connection,
    filter: &QueryFilter,
    args: &mut Vec<rusqlite::types::Value>,
) -> Result<String> {
    let field = Field::resolve(conn, &filter.field)?;
    // Summary month links share the date field and can use the indexed month.
    if matches!(field, Field::Normalized(Role::Date))
        && filter.op == FilterOp::Prefix
        && filter.value.len() == 7
        && normalize_date(&format!("{}-01", filter.value)).is_some()
    {
        args.push(filter.value.clone().into());
        return Ok("r.month = ?".into());
    }
    let target = field.sql(args);
    let clause = match filter.op {
        FilterOp::Empty => match field {
            Field::Raw(_) => format!("base_blank({target})"),
            Field::Normalized(_) => format!("{target} IS NULL"),
        },
        FilterOp::Contains | FilterOp::Prefix => {
            let position = if filter.op == FilterOp::Contains {
                "> 0"
            } else {
                "= 1"
            };
            args.push(fold_text(&filter.value).into());
            format!("instr(base_fold({target}), ?) {position}")
        }
        FilterOp::Eq | FilterOp::From | FilterOp::To => {
            let operator = match filter.op {
                FilterOp::Eq => "=",
                FilterOp::From => ">=",
                FilterOp::To => "<=",
                _ => unreachable!(),
            };
            args.push(field.comparison_value(&filter.value)?.into());
            format!("{target} {operator} ?")
        }
    };
    Ok(clause)
}

fn search_terms(input: &str) -> String {
    let mut terms = Vec::new();
    let mut token = String::new();
    for ch in input.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() {
            token.push(ch);
        } else if !token.is_empty() {
            // SQL binding keeps this text out of SQL and quoted words keep it
            // out of the FTS expression grammar.
            let suffix = if ch == '*' { "*" } else { "" };
            terms.push(format!("\"{token}\"{suffix}"));
            token.clear();
        }
    }
    terms.join(" AND ")
}
