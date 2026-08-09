// Results grid: sticky header, horizontal scroll, column picker, sortable
// headers, per-cell tooltips, duplicate highlighting, EDRPOU links, and a
// right-click actions menu (copy value/row, search this value, open company).

import { useEffect, useMemo, useState } from "react";

import type { FieldDto, ResultSort, RowDto } from "../api/types";
import { Icon } from "./Icon";
import { useI18n } from "../lib/i18n";
import { copyText } from "../lib/clipboard";

interface MenuState {
  x: number;
  y: number;
  field: FieldDto;
  value: string;
  rowId: number;
}

const RESULT_NUMBER = new Intl.NumberFormat("en-US", {
  maximumFractionDigits: 6,
  useGrouping: true,
});

export function formatResultCell(field: FieldDto, value: string): string {
  if (field.kind !== "number" || value.trim() === "") return value;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? RESULT_NUMBER.format(parsed) : value;
}

export function ResultsTable({
  fields,
  rows,
  onOpen,
  sort,
  onSortChange,
  companyFieldId,
  onOpenCompany,
  onSearchValue,
  onCopied,
}: {
  fields: FieldDto[];
  rows: RowDto[];
  onOpen: (id: number) => void;
  sort?: ResultSort | null;
  onSortChange?: (sort: ResultSort | null) => void;
  companyFieldId?: string | null;
  onOpenCompany?: (edrpou: string) => void;
  onSearchValue?: (field: FieldDto, value: string) => void;
  onCopied?: (ok: boolean) => void;
}) {
  const { t } = useI18n();
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [pickerOpen, setPickerOpen] = useState(false);
  const [menu, setMenu] = useState<MenuState | null>(null);

  const visible = useMemo(
    () =>
      fields
        .map((f, i) => ({ field: f, index: i }))
        .filter((c) => !hidden.has(c.field.id)),
    [fields, hidden],
  );

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  const toggle = (id: string) =>
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const cycleSort = (fieldId: string) => {
    if (!onSortChange) return;
    if (!sort || sort.field !== fieldId) {
      onSortChange({ field: fieldId, descending: false });
    } else if (!sort.descending) {
      onSortChange({ field: fieldId, descending: true });
    } else {
      onSortChange(null);
    }
  };

  const copy = async (text: string) => onCopied?.(await copyText(text));

  return (
    <div className="stack" style={{ gap: 10 }}>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <div className="popover">
          <button className="btn btn-sm btn-ghost" onClick={() => setPickerOpen((v) => !v)}>
            <Icon name="columns" size={15} /> {t("search_columns")} ({visible.length}/
            {fields.length})
          </button>
          {pickerOpen ? (
            <div className="popover-panel" onMouseLeave={() => setPickerOpen(false)}>
              {fields.map((f) => (
                <label key={f.id} className="check-row">
                  <input
                    type="checkbox"
                    checked={!hidden.has(f.id)}
                    onChange={() => toggle(f.id)}
                  />
                  <span>{f.label}</span>
                </label>
              ))}
            </div>
          ) : null}
        </div>
      </div>

      <div className="table-wrap">
        <table className="grid">
          <thead>
            <tr>
              {visible.map((c) => {
                const active = sort?.field === c.field.id;
                return (
                  <th
                    key={c.field.id}
                    title={c.field.label}
                    onClick={() => cycleSort(c.field.id)}
                    style={{ cursor: onSortChange ? "pointer" : "default" }}
                  >
                    <span className="row" style={{ gap: 5 }}>
                      {c.field.label}
                      {active ? (
                        <span style={{ color: "var(--text-dim)" }}>
                          {sort?.descending ? "▼" : "▲"}
                        </span>
                      ) : null}
                    </span>
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr
                key={row.id}
                className={row.duplicate_of ? "dup" : ""}
                onClick={() => onOpen(row.id)}
              >
                {visible.map((c, ci) => {
                  const value = row.values[c.index] ?? "";
                  const displayValue = formatResultCell(c.field, value);
                  const isCompany =
                    companyFieldId != null && c.field.id === companyFieldId && value.trim() !== "";
                  return (
                    <td
                      key={c.field.id}
                      title={displayValue}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        setMenu({ x: e.clientX, y: e.clientY, field: c.field, value, rowId: row.id });
                      }}
                    >
                      {isCompany && onOpenCompany ? (
                        <button
                          className="link-cell"
                          onClick={(e) => {
                            e.stopPropagation();
                            onOpenCompany(value);
                          }}
                        >
                          {displayValue}
                        </button>
                      ) : (
                        displayValue
                      )}
                      {ci === 0 && row.duplicate_of ? (
                        <span
                          className="dup-tag"
                          title={t("results_duplicate_of", { id: row.duplicate_of })}
                        >
                          {t("search_duplicate")}
                        </span>
                      ) : null}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {menu ? (
        <div
          className="context-menu"
          style={{ left: menu.x, top: menu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <button className="context-item" onClick={() => { copy(menu.value); setMenu(null); }}>
            {t("results_copy_value")}
          </button>
          <button
            className="context-item"
            onClick={() => {
              const row = rows.find((r) => r.id === menu.rowId);
              if (row) copy(fields.map((_, i) => row.values[i] ?? "").join("\t"));
              setMenu(null);
            }}
          >
            {t("results_copy_row")}
          </button>
          {onSearchValue && menu.value.trim() ? (
            <button
              className="context-item"
              onClick={() => {
                onSearchValue(menu.field, menu.value);
                setMenu(null);
              }}
            >
              {t("results_search_value")}
            </button>
          ) : null}
          {onOpenCompany && menu.field.id === companyFieldId && menu.value.trim() ? (
            <button
              className="context-item"
              onClick={() => {
                onOpenCompany(menu.value);
                setMenu(null);
              }}
            >
              {t("results_open_company")}
            </button>
          ) : null}
          <button className="context-item" onClick={() => { onOpen(menu.rowId); setMenu(null); }}>
            {t("results_open_record")}
          </button>
        </div>
      ) : null}
    </div>
  );
}
