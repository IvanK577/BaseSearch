use std::collections::{BTreeMap, VecDeque};

use chrono::{Months, NaiveDate};
use rusqlite::params_from_iter;
use rust_decimal::Decimal;

use crate::model::{
    CurrencyTotal, Dimension, FilterOp, QueryFilter, QuerySpec, Summary, SummaryGroup,
};
use crate::query::{Plan, snapshot};
use crate::workspace::{Workspace, display_decimal, exact_decimal, fold_text};
use crate::{Error, Result};

const GROUP_LIMIT: usize = 200;

pub fn summarize(
    workspace: &Workspace,
    query: &QuerySpec,
    dimension: Dimension,
) -> Result<Summary> {
    let mut conn = workspace.connect()?;
    let tx = conn.transaction()?;
    snapshot(&tx, query.revision)?;
    let plan = Plan::new(&tx, query)?;
    let key_column = match dimension {
        Dimension::Month => "r.month",
        Dimension::Company => "r.company",
        Dimension::Product => "r.product",
    };
    let sql = format!(
        "SELECT {key_column}, r.currency, r.amount, r.weight_kg, r.invalid_amount,
         r.unknown_weight_unit FROM records AS r{} ORDER BY {key_column} ASC NULLS LAST, r.id ASC",
        plan.condition,
    );
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(&plan.args))?;
    let mut current = None::<Group>;
    let mut summary = SummaryBuilder::new(dimension);
    while let Some(row) = rows.next()? {
        let key = row.get::<_, Option<String>>(0)?;
        if current
            .as_ref()
            .is_some_and(|group| !group.matches(key.as_deref()))
            && let Some(group) = current.take()
        {
            summary.push(group)?;
        }
        let group = current.get_or_insert_with(|| Group::new(key));
        group.totals.add_row(
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        )?;
    }
    if let Some(group) = current {
        summary.push(group)?;
    }
    Ok(summary.finish())
}

#[derive(Default)]
struct Totals {
    rows: u64,
    currency: BTreeMap<Option<String>, Decimal>,
    weight: Option<Decimal>,
    unknown_values: u64,
    unknown_units: u64,
}

impl Totals {
    fn add_row(
        &mut self,
        currency: Option<String>,
        amount: Option<String>,
        weight: Option<String>,
        unknown_value: bool,
        unknown_unit: bool,
    ) -> Result<()> {
        self.rows += 1;
        self.unknown_values += u64::from(unknown_value);
        self.unknown_units += u64::from(unknown_unit);
        if let Some(amount) = amount {
            self.push_amount(currency, stored_decimal(&amount)?)?;
        }
        if let Some(weight) = weight {
            self.push_weight(stored_decimal(&weight)?)?;
        }
        Ok(())
    }

    fn merge(&mut self, group: &Totals) -> Result<()> {
        self.rows += group.rows;
        self.unknown_values += group.unknown_values;
        self.unknown_units += group.unknown_units;
        for (currency, amount) in &group.currency {
            self.push_amount(currency.clone(), *amount)?;
        }
        if let Some(weight) = group.weight {
            self.push_weight(weight)?;
        }
        Ok(())
    }

    fn push_amount(&mut self, currency: Option<String>, amount: Decimal) -> Result<()> {
        let total = self.currency.entry(currency).or_default();
        *total = add_exact(*total, amount)?;
        Ok(())
    }

    fn push_weight(&mut self, weight: Decimal) -> Result<()> {
        self.weight = Some(add_exact(self.weight.unwrap_or_default(), weight)?);
        Ok(())
    }

    fn currency_totals(&self) -> Vec<CurrencyTotal> {
        self.currency
            .iter()
            .map(|(currency, amount)| CurrencyTotal {
                currency: currency.clone(),
                amount: display_decimal(*amount),
            })
            .collect()
    }
}

struct Group {
    display: Option<String>,
    folded: Option<String>,
    totals: Totals,
}

impl Group {
    fn new(display: Option<String>) -> Self {
        let folded = display.as_deref().map(fold_text);
        Self {
            display,
            folded,
            totals: Totals::default(),
        }
    }

    fn matches(&self, key: Option<&str>) -> bool {
        self.display.as_deref() == key || self.folded == key.map(fold_text)
    }
}

struct SummaryBuilder {
    dimension: Dimension,
    totals: Totals,
    groups: VecDeque<SummaryGroup>,
    total_groups: u64,
    previous_month: Option<(NaiveDate, u64)>,
}

impl SummaryBuilder {
    fn new(dimension: Dimension) -> Self {
        Self {
            dimension,
            totals: Totals::default(),
            groups: VecDeque::with_capacity(GROUP_LIMIT),
            total_groups: 0,
            previous_month: None,
        }
    }

    fn push(&mut self, group: Group) -> Result<()> {
        self.totals.merge(&group.totals)?;
        self.total_groups += 1;
        let rows = group.totals.rows;
        let (field, op, position, change_percent) = match self.dimension {
            Dimension::Month => {
                let month = group.display.as_ref().and_then(|key| {
                    NaiveDate::parse_from_str(&format!("{key}-01"), "%Y-%m-%d").ok()
                });
                let change = match (month, self.previous_month) {
                    (Some(month), Some((previous, baseline)))
                        if month.checked_sub_months(Months::new(1)) == Some(previous) =>
                    {
                        Some((rows as f64 / baseline as f64 - 1.0) * 100.0)
                    }
                    _ => None,
                };
                self.previous_month = month.map(|day| (day, rows));
                if self.groups.len() == GROUP_LIMIT {
                    self.groups.pop_front();
                }
                ("@date", FilterOp::Prefix, self.groups.len(), change)
            }
            Dimension::Company | Dimension::Product => {
                let position = self.groups.partition_point(|item| item.rows >= rows);
                if position == GROUP_LIMIT {
                    return Ok(());
                }
                if self.groups.len() == GROUP_LIMIT {
                    self.groups.pop_back();
                }
                let field = if self.dimension == Dimension::Company {
                    "@company"
                } else {
                    "@product"
                };
                (field, FilterOp::Eq, position, None)
            }
        };
        let filter = QueryFilter {
            field: field.into(),
            op: if group.display.is_some() {
                op
            } else {
                FilterOp::Empty
            },
            value: group.display.clone().unwrap_or_default(),
        };
        self.groups.insert(
            position,
            SummaryGroup {
                key: group.display.unwrap_or_default(),
                rows,
                share_percent: 0.0,
                change_percent,
                currency_totals: group.totals.currency_totals(),
                net_weight_kg: group.totals.weight.map(display_decimal),
                filter,
            },
        );
        Ok(())
    }

    fn finish(self) -> Summary {
        let total_rows = self.totals.rows;
        let groups = self
            .groups
            .into_iter()
            .map(|mut group| {
                group.share_percent = group.rows as f64 / total_rows as f64 * 100.0;
                group
            })
            .collect();
        Summary {
            total_rows,
            currency_totals: self.totals.currency_totals(),
            net_weight_kg: self.totals.weight.map(display_decimal),
            unknown_value_rows: self.totals.unknown_values,
            unknown_unit_rows: self.totals.unknown_units,
            total_groups: self.total_groups,
            groups,
        }
    }
}

fn stored_decimal(text: &str) -> Result<Decimal> {
    Decimal::from_str_exact(text)
        .map_err(|_| Error::Message("В хранилище обнаружено повреждённое число.".into()))
}

fn add_exact(left: Decimal, right: Decimal) -> Result<Decimal> {
    let scale = left.scale().max(right.scale());
    let aligned = |value: Decimal| {
        value
            .mantissa()
            .checked_mul(10_i128.pow(scale - value.scale()))
    };
    let sum = aligned(left)
        .zip(aligned(right))
        .and_then(|(left, right)| left.checked_add(right))
        .and_then(|mantissa| exact_decimal(mantissa, scale));
    sum.ok_or_else(|| Error::Message("Точная сумма выходит за допустимый диапазон.".into()))
}
