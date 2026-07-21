import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, ApiError } from "../api/client";
import type {
  FieldDto,
  Filters,
  Query,
  QueryExpr,
  ResultSort,
  SchemaResponse,
  SearchResponse,
} from "../api/types";
import { AdvancedBuilder } from "../components/AdvancedBuilder";
import { Icon } from "../components/Icon";
import { RecordDrawer } from "../components/RecordDrawer";
import { ResultsTable } from "../components/ResultsTable";
import { Banner, EmptyState, Loading } from "../components/ui";
import { useI18n } from "../lib/i18n";
import { readActiveResultSort, writeActiveResultSort } from "../lib/exportContext";
import { navigate, updateRouteQuery, useRouteQuery } from "../lib/router";
import { formatInt } from "../lib/format";
import {
  recordSearch,
  removeSaved,
  saveSearch,
  useSearches,
} from "../lib/savedSearches";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

const LIMIT = 50;

const FILTER_FIELDS: { key: keyof Filters; label: string }[] = [
  { key: "year", label: "Year" },
  { key: "product_code", label: "Product code" },
  { key: "edrpou", label: "Company code (EDRPOU)" },
  { key: "recipient", label: "Recipient" },
  { key: "sender", label: "Sender" },
  { key: "trademark", label: "Trademark" },
  { key: "description", label: "Description" },
  { key: "origin_country", label: "Origin country" },
  { key: "dispatch_country", label: "Dispatch country" },
  { key: "trade_country", label: "Trade country" },
];

// Result columns that map onto a structured filter, for "search this value".
const COLUMN_TO_FILTER: Record<string, keyof Filters> = {
  edrpou: "edrpou",
  product_code: "product_code",
  recipient: "recipient",
  sender: "sender",
  trademark: "trademark",
  description: "description",
  origin_country: "origin_country",
  dispatch_country: "dispatch_country",
  trade_country: "trade_country",
  year: "year",
};

export function SearchPage() {
  const { t } = useI18n();
  const {
    draftQuery: query,
    query: appliedQuery,
    setQuery,
    commit,
    setText,
    setFilter,
    setAdvanced,
    setRecordScope,
    reset,
    isEmpty,
    isDirty,
  } = useQueryStore();
  const { openCompany, toast, status } = useStore();
  const { recent, saved } = useSearches();
  const [results, setResults] = useState<SearchResponse | null>(null);
  const [total, setTotal] = useState<number | null>(null);
  const [offset, setOffset] = useState(0);
  const [sort, setSort] = useState<ResultSort | null>(readActiveResultSort);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [showFilters, setShowFilters] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [showSaved, setShowSaved] = useState(false);
  const [schema, setSchema] = useState<SchemaResponse | null>(null);
  const drawerRecord = useRouteQuery("record");
  const drawerId = drawerRecord && /^\d+$/.test(drawerRecord) ? Number(drawerRecord) : null;
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api
      .schema()
      .then((response) => {
        setSchema(response);
        setSort((current) => {
          if (!current || response.result_fields.some((field) => field.id === current.field)) {
            return current;
          }
          writeActiveResultSort(null);
          return null;
        });
      })
      .catch(() => {});
  }, []);

  const companyFieldId = useMemo(() => {
    if (!schema) return null;
    if (schema.has_shape) {
      const col = schema.columns.find((c) => c.semantic === "CompanyCode");
      return col ? `source:${col.id}` : null;
    }
    return "edrpou";
  }, [schema]);

  const runSearch = useCallback(
    async (nextOffset: number, useSort: ResultSort | null, q: Query) => {
      setLoading(true);
      setError(null);
      const [searchRes, countRes] = await Promise.allSettled([
        api.search(q, LIMIT, nextOffset, useSort),
        api.count(q),
      ]);
      if (searchRes.status === "fulfilled") {
        setResults(searchRes.value);
        setOffset(nextOffset);
        recordSearch(q);
      } else {
        setError((searchRes.reason as ApiError)?.message ?? "Search failed");
        setResults(null);
      }
      setTotal(countRes.status === "fulfilled" ? countRes.value.total : null);
      setLoading(false);
      setSearched(true);
    },
    [],
  );

  const submit = () => {
    commit(query);
    runSearch(0, sort, query);
  };

  const changeSort = (next: ResultSort | null) => {
    setSort(next);
    writeActiveResultSort(next);
    runSearch(0, next, appliedQuery);
  };

  const loadEntryQuery = (entryQuery: Query) => {
    setQuery(entryQuery);
    setShowSaved(false);
    runSearch(0, sort, entryQuery);
  };

  const searchValue = (field: FieldDto, value: string) => {
    const columnName = field.source.kind === "column" ? field.source.name : null;
    const filterKey = columnName ? COLUMN_TO_FILTER[columnName] : undefined;
    let next: Query;
    if (filterKey) {
      next = {
        ...appliedQuery,
        filters: { ...appliedQuery.filters, [filterKey]: value },
      };
    } else {
      next = { ...appliedQuery, text: value };
    }
    setQuery(next);
    runSearch(0, sort, next);
  };

  useEffect(() => {
    if (!isEmpty && !searched) {
      runSearch(0, sort, appliedQuery);
    }
    // This restores the last applied search when the user returns to the page.
    // Subsequent query changes are run only by explicit actions above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Global "/" focuses the search box.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "/" && drawerId === null) {
        const tag = (e.target as HTMLElement)?.tagName;
        if (tag !== "INPUT" && tag !== "TEXTAREA" && tag !== "SELECT") {
          e.preventDefault();
          inputRef.current?.focus();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [drawerId]);

  const onAdvancedChange = useCallback(
    (expr: QueryExpr | null) => setAdvanced(expr),
    [setAdvanced],
  );

  const activeFilters = FILTER_FIELDS.filter((f) => query.filters[f.key].trim());

  return (
    <div className="stack">
      <div className="panel panel-pad stack" style={{ gap: 14 }}>
        <div className="searchbar">
          <div className="input-wrap">
            <span className="search-icon">
              <Icon name="search" size={18} />
            </span>
            <input
              ref={inputRef}
              className="input"
              placeholder={t("search_placeholder")}
              value={query.text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submit()}
            />
          </div>
          <button className="btn btn-primary" onClick={submit} disabled={loading}>
            {t("search_run")}
          </button>
        </div>

        {isDirty ? (
          <div className="faint" role="status">
            Search controls changed. Run the search to apply them to results and analytics.
          </div>
        ) : null}

        <div className="toolbar">
          <button
            className={`btn btn-sm ${showFilters ? "" : "btn-ghost"}`}
            onClick={() => setShowFilters((v) => !v)}
          >
            <Icon name="filter" size={15} /> {t("search_filters")}
            {activeFilters.length > 0 ? ` (${activeFilters.length})` : ""}
          </button>
          <button
            className={`btn btn-sm ${showAdvanced ? "" : "btn-ghost"}`}
            onClick={() => setShowAdvanced((v) => !v)}
          >
            <Icon name="columns" size={15} /> {t("search_advanced")}
            {query.advanced ? " •" : ""}
          </button>

          <div className="popover">
            <button className="btn btn-sm btn-ghost" onClick={() => setShowSaved((v) => !v)}>
              <Icon name="jobs" size={15} /> Saved & recent
            </button>
            {showSaved ? (
              <div
                className="popover-panel"
                style={{ width: 320, left: 0, right: "auto" }}
                onMouseLeave={() => setShowSaved(false)}
              >
                <SavedList
                  saved={saved}
                  recent={recent}
                  onPick={loadEntryQuery}
                  onRemove={removeSaved}
                />
              </div>
            ) : null}
          </div>

          <button
            className="btn btn-sm btn-ghost"
            title="Save current search"
            onClick={() => {
              const name = window.prompt("Name this search (optional):") ?? undefined;
              saveSearch(query, name);
              toast("Search saved", "success");
            }}
          >
            <Icon name="plus" size={15} /> Save
          </button>

          <div className="grow" />
          <label className="field-inline">
            <span className="field-label">{t("search_scope")}</span>
            <select
              className="input input-compact"
              value={query.record_scope ?? "canonical"}
              onChange={(event) =>
                setRecordScope(event.target.value as "canonical" | "occurrences")
              }
            >
              <option value="canonical">{t("search_scope_canonical")}</option>
              <option value="occurrences">{t("search_scope_occurrences")}</option>
            </select>
          </label>
          <button
            className="btn btn-sm btn-ghost"
            onClick={() => {
              reset();
              setResults(null);
              setTotal(null);
              setSearched(false);
              setSort(null);
              writeActiveResultSort(null);
            }}
          >
            {t("common_reset")}
          </button>
          <button className="btn btn-sm" onClick={() => navigate("exports")}>
            <Icon name="export" size={15} /> {t("search_export")}
          </button>
        </div>

        {showFilters ? (
          <div className="filters-grid">
            {FILTER_FIELDS.map((f) => (
              <div key={f.key}>
                <label className="field-label">{f.label}</label>
                <input
                  className="input"
                  value={query.filters[f.key]}
                  onChange={(e) => setFilter(f.key, e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && submit()}
                />
              </div>
            ))}
          </div>
        ) : null}

        {showAdvanced && schema ? (
          <div className="stack" style={{ gap: 10 }}>
            <div className="section-title" style={{ margin: 0 }}>
              {t("search_advanced")}
            </div>
            <AdvancedBuilder
              fields={schema.search_fields}
              value={query.advanced}
              onChange={onAdvancedChange}
            />
          </div>
        ) : null}
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading && !results ? <Loading label={t("common_loading")} /> : null}
      {!loading && !searched ? (
        status && !status.has_data ? (
          <EmptyState
            icon="import"
            title={t("welcome_no_data")}
            hint={t("imports_hint")}
            action={
              <button className="btn btn-primary" onClick={() => navigate("imports")}>
                <Icon name="import" size={16} /> {t("welcome_import")}
              </button>
            }
          />
        ) : (
          <EmptyState icon="search" title={t("search_start")} />
        )
      ) : null}
      {results && results.rows.length === 0 && !loading ? (
        <EmptyState icon="search" title={t("search_empty")} />
      ) : null}

      {results && results.rows.length > 0 ? (
        <div className="stack" style={{ gap: 12 }}>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <div className="muted">
              {t("search_found")}:{" "}
              <strong style={{ color: "var(--text)" }}>
                {/* When the count request failed, admit it — a page size
                    presented as the total would be a lie. */}
                {total !== null
                  ? formatInt(total)
                  : `${formatInt(results.rows.length)}+`}
              </strong>{" "}
              {t("common_rows")}
              <span className="faint">
                {"  ·  "}
                {formatInt(offset + 1)}–{formatInt(offset + results.rows.length)}
              </span>
            </div>
            <div className="row" style={{ gap: 8 }}>
              <button
                className="btn btn-sm btn-ghost"
                disabled={offset === 0 || loading}
                onClick={() => runSearch(Math.max(0, offset - LIMIT), sort, appliedQuery)}
              >
                Prev
              </button>
              <button
                className="btn btn-sm btn-ghost"
                disabled={!results.has_next || loading}
                onClick={() => runSearch(offset + LIMIT, sort, appliedQuery)}
              >
                Next
              </button>
            </div>
          </div>

          <ResultsTable
            fields={results.fields}
            rows={results.rows}
            onOpen={(id) => updateRouteQuery("record", String(id))}
            sort={sort}
            onSortChange={changeSort}
            companyFieldId={companyFieldId}
            onOpenCompany={openCompany}
            onSearchValue={searchValue}
            onCopied={(ok) => toast(ok ? "Copied" : "Copy blocked", ok ? "success" : "error")}
          />
        </div>
      ) : null}

      {drawerId !== null ? (
        <RecordDrawer id={drawerId} onClose={() => updateRouteQuery("record", null)} />
      ) : null}
    </div>
  );
}

function SavedList({
  saved,
  recent,
  onPick,
  onRemove,
}: {
  saved: ReturnType<typeof useSearches>["saved"];
  recent: ReturnType<typeof useSearches>["recent"];
  onPick: (query: Query) => void;
  onRemove: (id: string) => void;
}) {
  return (
    <div className="stack" style={{ gap: 4 }}>
      {saved.length > 0 ? (
        <>
          <div className="field-label" style={{ margin: "2px 4px" }}>
            Saved
          </div>
          {saved.map((e) => (
            <div key={e.id} className="check-row" style={{ justifyContent: "space-between" }}>
              <button
                className="btn btn-ghost btn-sm"
                style={{ flex: 1, textAlign: "left", justifyContent: "flex-start" }}
                onClick={() => onPick(e.query)}
              >
                {e.label}
              </button>
              <button className="btn btn-ghost btn-sm" onClick={() => onRemove(e.id)}>
                <Icon name="trash" size={14} />
              </button>
            </div>
          ))}
        </>
      ) : null}
      <div className="field-label" style={{ margin: "6px 4px 2px" }}>
        Recent
      </div>
      {recent.length === 0 ? (
        <div className="faint" style={{ padding: "4px 8px" }}>
          No recent searches yet.
        </div>
      ) : (
        recent.map((e) => (
          <button
            key={e.id}
            className="btn btn-ghost btn-sm"
            style={{ textAlign: "left", justifyContent: "flex-start" }}
            onClick={() => onPick(e.query)}
          >
            {e.label}
          </button>
        ))
      )}
    </div>
  );
}
