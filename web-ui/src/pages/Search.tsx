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
import { FILTER_FIELDS } from "../lib/filterFields";
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
  const requestIdRef = useRef(0);
  // Snapshot token from the last successful search, reused while paging.
  const snapshotRef = useRef<number | null>(null);

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
    async (
      nextOffset: number,
      useSort: ResultSort | null,
      q: Query,
      reuseSnapshot = false,
    ) => {
      const requestId = ++requestIdRef.current;
      setLoading(true);
      setError(null);
      // /api/search already returns the total and the snapshot token, so a
      // second /api/count round trip would just repeat the most expensive part
      // of the same query. Paging reuses the token, which also stops the row
      // set from shifting under the user while an import is running.
      const snapshot = reuseSnapshot ? (snapshotRef.current ?? undefined) : undefined;
      const searchRes = await api
        .search(q, LIMIT, nextOffset, useSort, snapshot)
        .then((value) => ({ ok: true as const, value }))
        .catch((reason: unknown) => ({ ok: false as const, reason }));
      if (requestId !== requestIdRef.current) return;
      if (searchRes.ok) {
        snapshotRef.current = searchRes.value.snapshot;
        setResults(searchRes.value);
        setTotal(searchRes.value.total);
        setOffset(nextOffset);
        recordSearch(q);
      } else {
        setError((searchRes.reason as ApiError)?.message ?? t("search_failed"));
        setResults(null);
        setTotal(null);
      }
      setLoading(false);
      setSearched(true);
    },
    [t],
  );

  useEffect(
    () => () => {
      requestIdRef.current += 1;
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
      <section className="panel search-workbench" aria-label={t("nav_search")}>
        <div className="searchbar search-command">
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
            <Icon name="search" size={16} />
            {t("search_run")}
          </button>
        </div>

        <div className="search-options">
          <div className="toolbar search-actions">
            <button
              className={`btn btn-sm ${showFilters ? "" : "btn-ghost"}`}
              onClick={() => setShowFilters((value) => !value)}
              aria-expanded={showFilters}
            >
              <Icon name="filter" size={15} /> {t("search_filters")}
              {activeFilters.length > 0 ? ` (${activeFilters.length})` : ""}
            </button>
            <button
              className={`btn btn-sm ${showAdvanced ? "" : "btn-ghost"}`}
              onClick={() => setShowAdvanced((value) => !value)}
              aria-expanded={showAdvanced}
            >
              <Icon name="columns" size={15} /> {t("search_advanced")}
              {query.advanced ? " •" : ""}
            </button>

            <div className="popover">
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => setShowSaved((value) => !value)}
                aria-expanded={showSaved}
              >
                <Icon name="bookmark" size={15} /> {t("search_saved_recent")}
              </button>
              {showSaved ? (
                <div
                  className="popover-panel saved-searches-popover"
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
              title={t("search_save_current")}
              onClick={() => {
                const name = window.prompt(t("search_name_prompt")) ?? undefined;
                saveSearch(query, name);
                toast(t("search_saved_toast"), "success");
              }}
            >
              <Icon name="bookmark" size={15} /> {t("search_save_current")}
            </button>
          </div>

          <div className="toolbar search-scope-actions">
            <div className="segmented-control" aria-label={t("search_scope")}>
              <button
                type="button"
                className={query.record_scope !== "occurrences" ? "active" : ""}
                onClick={() => setRecordScope("canonical")}
                aria-pressed={query.record_scope !== "occurrences"}
              >
                {t("search_scope_canonical")}
              </button>
              <button
                type="button"
                className={query.record_scope === "occurrences" ? "active" : ""}
                onClick={() => setRecordScope("occurrences")}
                aria-pressed={query.record_scope === "occurrences"}
              >
                {t("search_scope_occurrences")}
              </button>
            </div>
            <button
              className="icon-button"
              onClick={() => {
                requestIdRef.current += 1;
                setLoading(false);
                setError(null);
                reset();
                setResults(null);
                setTotal(null);
                setSearched(false);
                setSort(null);
                writeActiveResultSort(null);
              }}
              aria-label={t("common_reset")}
              title={t("common_reset")}
            >
              <Icon name="refresh" size={15} />
            </button>
            <button className="btn btn-sm" onClick={() => navigate("exports")}>
              <Icon name="export" size={15} /> {t("search_export")}
            </button>
          </div>
        </div>

        {isDirty ? (
          <div className="query-dirty" role="status">
            {t("search_dirty")}
          </div>
        ) : null}

        {showFilters ? (
          <div className="filters-grid">
            {FILTER_FIELDS.map((f) => (
              <div key={f.key}>
                <label className="field-label">{t(f.labelKey)}</label>
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
      </section>

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
          <div className="results-summary">
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
            <div className="row pagination-controls" style={{ gap: 4 }}>
              <button
                className="icon-button"
                disabled={offset === 0 || loading}
                onClick={() => runSearch(Math.max(0, offset - LIMIT), sort, appliedQuery, true)}
                aria-label={t("search_previous_page")}
                title={t("search_previous_page")}
              >
                <Icon name="arrow-left" size={16} />
              </button>
              <button
                className="icon-button"
                disabled={!results.has_next || loading}
                onClick={() => runSearch(offset + LIMIT, sort, appliedQuery, true)}
                aria-label={t("search_next_page")}
                title={t("search_next_page")}
              >
                <Icon name="arrow-right" size={16} />
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
            onCopied={(ok) =>
              toast(ok ? t("report_copied") : t("report_copy_failed"), ok ? "success" : "error")
            }
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
  const { t } = useI18n();
  return (
    <div className="stack" style={{ gap: 4 }}>
      {saved.length > 0 ? (
        <>
          <div className="field-label" style={{ margin: "2px 4px" }}>
            {t("search_saved_label")}
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
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => onRemove(e.id)}
                aria-label={t("search_remove_saved", { name: e.label })}
                title={t("search_remove_saved", { name: e.label })}
              >
                <Icon name="trash" size={14} />
              </button>
            </div>
          ))}
        </>
      ) : null}
      <div className="field-label" style={{ margin: "6px 4px 2px" }}>
        {t("search_recent_label")}
      </div>
      {recent.length === 0 ? (
        <div className="faint" style={{ padding: "4px 8px" }}>
          {t("search_no_recent")}
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
