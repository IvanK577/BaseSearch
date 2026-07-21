import { useEffect, useMemo, useState } from "react";

import { api, ApiError, apiUrl } from "../api/client";
import type {
  ExportJobResult,
  FieldDto,
  Job,
  ResultSort,
  SchemaResponse,
} from "../api/types";
import { Icon } from "../components/Icon";
import { Progress } from "../components/ui";
import {
  describeExportQuery,
  moveFieldId,
  readActiveResultSort,
  selectAllFieldIds,
  writeActiveResultSort,
} from "../lib/exportContext";
import { formatBytes, formatInt } from "../lib/format";
import type { MessageKey, Translate } from "../lib/i18n";
import { useI18n } from "../lib/i18n";
import { jobPhaseLabel, jobTitleLabel } from "../lib/jobText";
import { navigate } from "../lib/router";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

const XLSX_MAX_ROWS = 1_048_575;

const EXPORT_ERROR_KEYS: Partial<Record<string, MessageKey>> = {
  export_error_empty_fields: "export_error_empty_fields",
  export_error_too_many_fields: "export_error_too_many_fields",
  export_error_duplicate_field: "export_error_duplicate_field",
  export_error_unknown_field: "export_error_unknown_field",
  export_error_unknown_sort_field: "export_error_unknown_sort_field",
};

function exportErrorMessage(
  error: unknown,
  t: Translate,
  fallbackKey: MessageKey,
): string {
  if (!(error instanceof ApiError)) return t(fallbackKey);
  const localizedKey = EXPORT_ERROR_KEYS[error.code];
  if (localizedKey) return t(localizedKey);
  if (error.code === "network" || error.code === "internal") return t(fallbackKey);
  return error.message.trim() || t(fallbackKey);
}

export function ExportsPage() {
  const { lang, t } = useI18n();
  const { query } = useQueryStore();
  const { jobs, refreshJobs, toast } = useStore();
  const [format, setFormat] = useState<"csv" | "xlsx">("csv");
  const [filename, setFilename] = useState("");
  const [count, setCount] = useState<number | null>(null);
  const [countError, setCountError] = useState<string | null>(null);
  const [schema, setSchema] = useState<SchemaResponse | null>(null);
  const [fieldIds, setFieldIds] = useState<string[]>([]);
  const [sort, setSort] = useState<ResultSort | null>(readActiveResultSort);
  const [loadingSchema, setLoadingSchema] = useState(true);
  const [schemaError, setSchemaError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    let alive = true;
    setCount(null);
    setCountError(null);
    api
      .count(query)
      .then((response) => alive && setCount(response.total))
      .catch((error) => {
        if (alive) setCountError(exportErrorMessage(error, t, "exports_count_error"));
      });
    return () => {
      alive = false;
    };
  }, [query, lang]);

  useEffect(() => {
    let alive = true;
    api
      .schema()
      .then((response) => {
        if (!alive) return;
        setSchema(response);
        setFieldIds(response.result_fields.map((field) => field.id));
        setSort((current) => {
          if (!current || response.result_fields.some((field) => field.id === current.field)) {
            return current;
          }
          writeActiveResultSort(null);
          return null;
        });
        setSchemaError(null);
      })
      .catch((error) => {
        if (alive) {
          setSchemaError(exportErrorMessage(error, t, "exports_columns_load_error"));
        }
      })
      .finally(() => {
        if (alive) setLoadingSchema(false);
      });
    return () => {
      alive = false;
    };
  }, [lang]);

  const exportJobs = useMemo(
    () => jobs.filter((job) => job.kind === "export").slice(0, 6),
    [jobs],
  );
  const fieldMap = useMemo(
    () => new Map(schema?.result_fields.map((field) => [field.id, field]) ?? []),
    [schema],
  );
  const selectedFields = useMemo(
    () => fieldIds.flatMap((id) => (fieldMap.has(id) ? [fieldMap.get(id)!] : [])),
    [fieldIds, fieldMap],
  );
  const unselectedFields = useMemo(() => {
    const selected = new Set(fieldIds);
    return (schema?.result_fields ?? []).filter((field) => !selected.has(field.id));
  }, [fieldIds, schema]);
  const queryContext = useMemo(() => describeExportQuery(query, t), [query, lang]);
  const sortField = sort ? fieldMap.get(sort.field) : undefined;
  const sortDescription = sortField
    ? t(sort!.descending ? "exports_sort_descending" : "exports_sort_ascending", {
        field: sortField.label,
      })
    : t("exports_sort_default");
  const xlsxTooLarge = format === "xlsx" && count != null && count > XLSX_MAX_ROWS;
  const canReview =
    count != null && !loadingSchema && !schemaError && fieldIds.length > 0 && !xlsxTooLarge;

  const updateFormat = (next: "csv" | "xlsx") => {
    setFormat(next);
    setConfirming(false);
  };

  const toggleField = (id: string) => {
    setFieldIds((current) =>
      current.includes(id) ? current.filter((fieldId) => fieldId !== id) : [...current, id],
    );
    setConfirming(false);
  };

  const moveField = (id: string, offset: -1 | 1) => {
    setFieldIds((current) => moveFieldId(current, id, offset));
    setConfirming(false);
  };

  const create = async () => {
    setCreating(true);
    try {
      await api.createExport(
        query,
        format,
        filename.trim() || undefined,
        fieldIds,
        sort,
      );
      toast(t("exports_started"), "info");
      setConfirming(false);
      refreshJobs();
    } catch (error) {
      toast(exportErrorMessage(error, t, "exports_failed"), "error");
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="stack content-narrow">
      <div className="panel panel-pad stack" style={{ gap: 16 }}>
        <div>
          <div className="section-title" style={{ margin: 0 }}>
            {t("exports_title")}
          </div>
          <p className="muted" style={{ margin: "6px 0 0" }}>
            {t("exports_desc")}
          </p>
        </div>

        <section aria-labelledby="export-context-title" className="stack" style={{ gap: 8 }}>
          <div id="export-context-title" className="field-label" style={{ margin: 0 }}>
            {t("exports_context_title")}
          </div>
          <div className="row wrap" style={{ justifyContent: "space-between", alignItems: "start" }}>
            <div className="stack" style={{ gap: 4 }}>
              <strong>
                {countError
                  ? t("exports_row_count_unavailable")
                  : count === null
                    ? t("common_loading")
                    : `${formatInt(count)} ${t("common_rows")}`}
              </strong>
              <span className="muted">{queryContext.summary}</span>
              <span className="faint">{queryContext.scope}</span>
            </div>
            <button className="btn btn-sm btn-ghost" onClick={() => navigate("search")}>
              {t("nav_search")}
            </button>
          </div>
          <div className="row wrap" style={{ gap: 8 }}>
            <span className="chip">{t("exports_sort_label", { sort: sortDescription })}</span>
            <span className="chip">{t("exports_columns_count", { count: fieldIds.length })}</span>
          </div>
          {countError ? <div className="banner">{countError}</div> : null}
        </section>

        <div className="row wrap" style={{ gap: 12, alignItems: "flex-end" }}>
          <div>
            <label className="field-label">{t("exports_format")}</label>
            <select
              className="select"
              style={{ width: 150 }}
              value={format}
              onChange={(event) => updateFormat(event.target.value as "csv" | "xlsx")}
            >
              <option value="csv">CSV</option>
              <option value="xlsx">Excel (.xlsx)</option>
            </select>
          </div>
          <div className="grow">
            <label className="field-label">{t("exports_filename")}</label>
            <input
              className="input"
              placeholder={`base-search-export.${format}`}
              value={filename}
              onChange={(event) => {
                setFilename(event.target.value);
                setConfirming(false);
              }}
            />
          </div>
        </div>

        <section aria-labelledby="export-columns-title" className="stack" style={{ gap: 10 }}>
          <div className="row wrap" style={{ justifyContent: "space-between" }}>
            <div>
              <div id="export-columns-title" style={{ fontWeight: 650 }}>
                {t("exports_columns_title")}
              </div>
              <div className="faint">{t("exports_columns_hint")}</div>
            </div>
            <div className="row" style={{ gap: 4 }}>
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => {
                  setFieldIds(selectAllFieldIds(schema?.result_fields ?? [], fieldIds));
                  setConfirming(false);
                }}
                disabled={!schema}
              >
                {t("exports_select_all")}
              </button>
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => {
                  setFieldIds([]);
                  setConfirming(false);
                }}
                disabled={!schema}
              >
                {t("exports_select_none")}
              </button>
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => {
                  setFieldIds(schema?.result_fields.map((field) => field.id) ?? []);
                  setConfirming(false);
                }}
                disabled={!schema}
              >
                {t("common_reset")}
              </button>
            </div>
          </div>

          {schemaError ? <div className="banner">{schemaError}</div> : null}
          {loadingSchema ? <div className="muted">{t("common_loading")}</div> : null}
          {!loadingSchema && schema && schema.result_fields.length === 0 ? (
            <div className="muted">{t("exports_no_columns")}</div>
          ) : null}
          {!loadingSchema && selectedFields.length === 0 && unselectedFields.length > 0 ? (
            <div className="banner">{t("exports_select_one_column")}</div>
          ) : null}

          {schema && schema.result_fields.length > 0 ? (
            <div
              style={{
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-sm)",
                maxHeight: 360,
                overflow: "auto",
              }}
            >
              {[...selectedFields, ...unselectedFields].map((field) => {
                const selectedIndex = fieldIds.indexOf(field.id);
                const selected = selectedIndex >= 0;
                return (
                  <FieldSelectionRow
                    key={field.id}
                    field={field}
                    selected={selected}
                    canMoveUp={selected && selectedIndex > 0}
                    canMoveDown={selected && selectedIndex < fieldIds.length - 1}
                    onToggle={() => toggleField(field.id)}
                    onMoveUp={() => moveField(field.id, -1)}
                    onMoveDown={() => moveField(field.id, 1)}
                    t={t}
                  />
                );
              })}
            </div>
          ) : null}
        </section>

        {xlsxTooLarge ? (
          <div className="banner">{t("exports_xlsx_limit")}</div>
        ) : null}

        {confirming ? (
          <div
            className="banner stack"
            role="group"
            aria-label={t("exports_confirm_title")}
            style={{ gap: 10 }}
          >
            <strong>{t("exports_confirm_title")}</strong>
            <span>
              {t("exports_confirm_summary", {
                rows: formatInt(count ?? 0),
                columns: fieldIds.length,
                format: format.toUpperCase(),
              })}
            </span>
            <span className="faint">
              {queryContext.summary} · {sortDescription}
            </span>
            <div className="row wrap" style={{ justifyContent: "flex-end" }}>
              <button className="btn btn-sm" onClick={() => setConfirming(false)} disabled={creating}>
                {t("common_back")}
              </button>
              <button className="btn btn-primary" onClick={create} disabled={creating}>
                <Icon name="export" size={16} /> {t("exports_start")}
              </button>
            </div>
          </div>
        ) : (
          <div className="row" style={{ justifyContent: "flex-end" }}>
            <button
              className="btn btn-primary"
              onClick={() => setConfirming(true)}
              disabled={!canReview}
            >
              <Icon name="export" size={16} /> {t("exports_review")}
            </button>
          </div>
        )}
      </div>

      {exportJobs.length > 0 ? (
        <div className="panel panel-pad stack" style={{ gap: 12 }}>
          <div className="section-title" style={{ margin: 0 }}>
            {t("nav_exports")}
          </div>
          {exportJobs.map((job) => (
            <ExportJobRow key={job.id} job={job} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function FieldSelectionRow({
  field,
  selected,
  canMoveUp,
  canMoveDown,
  onToggle,
  onMoveUp,
  onMoveDown,
  t,
}: {
  field: FieldDto;
  selected: boolean;
  canMoveUp: boolean;
  canMoveDown: boolean;
  onToggle: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  t: Translate;
}) {
  return (
    <div
      className="row"
      style={{
        minHeight: 42,
        padding: "5px 9px",
        borderBottom: "1px solid var(--border)",
      }}
    >
      <label className="row grow" style={{ gap: 9 }}>
        <input type="checkbox" checked={selected} onChange={onToggle} />
        <span>{field.label}</span>
      </label>
      {selected ? (
        <div className="row" style={{ gap: 2 }}>
          <MoveButton
            label={t("exports_move_up", { field: field.label })}
            direction="up"
            disabled={!canMoveUp}
            onClick={onMoveUp}
          />
          <MoveButton
            label={t("exports_move_down", { field: field.label })}
            direction="down"
            disabled={!canMoveDown}
            onClick={onMoveDown}
          />
        </div>
      ) : null}
    </div>
  );
}

function MoveButton({
  label,
  direction,
  disabled,
  onClick,
}: {
  label: string;
  direction: "up" | "down";
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className="btn btn-sm btn-ghost"
      style={{ width: 30, padding: 5 }}
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
    >
      <span style={{ display: "inline-flex", transform: `rotate(${direction === "up" ? -90 : 90}deg)` }}>
        <Icon name="chevron" size={14} />
      </span>
    </button>
  );
}

function ExportJobRow({ job }: { job: Job }) {
  const { t } = useI18n();
  const active = job.status === "running" || job.status === "queued";
  const result = job.result as ExportJobResult | undefined;
  return (
    <div className="stack" style={{ gap: 6 }}>
      <div className="row" style={{ justifyContent: "space-between" }}>
        <span>{jobTitleLabel(t, job)}</span>
        <span className="faint">
          {jobPhaseLabel(t, active ? job.progress.phase : "", job.status)}
        </span>
      </div>
      {active ? <Progress percent={job.progress.total > 0 ? job.progress.percent : null} /> : null}
      {result && job.status === "succeeded" ? (
        <div className="row wrap" style={{ justifyContent: "space-between" }}>
          <span className="faint">
            {formatInt(result.rows)} {t("common_rows")} · {t("exports_job_columns", {
              count: result.fields?.length ?? result.field_ids?.length ?? 0,
            })} · {formatBytes(result.bytes)}
          </span>
          <a className="btn btn-sm" href={apiUrl(result.download_url)}>
            <Icon name="download" size={15} /> {t("common_download")}
          </a>
        </div>
      ) : null}
      {job.error ? <div className="banner">{job.error}</div> : null}
    </div>
  );
}
