import { useEffect, useState } from "react";

import { api, ApiError } from "../api/client";
import type { SchemaResponse, SemanticField, SourceColumn } from "../api/types";
import { EmptyState, Loading } from "../components/ui";
import { useI18n } from "../lib/i18n";
import { columnRoleLabel, SEMANTICS } from "../lib/semantics";
import { useStore } from "../state/store";

export function ColumnsPage() {
  const { t } = useI18n();
  const { toast, canEditData } = useStore();
  const [schema, setSchema] = useState<SchemaResponse | null>(null);
  const [loading, setLoading] = useState(true);

  const reload = () => {
    setLoading(true);
    api
      .schema()
      .then(setSchema)
      .catch((err) =>
        toast((err as ApiError)?.message ?? t("columns_load_failed"), "error"),
      )
      .finally(() => setLoading(false));
  };

  useEffect(reload, []);

  const change = async (column: SourceColumn, value: string) => {
    const semantic = value === "" ? null : (value as SemanticField);
    try {
      const res = await api.setColumnSemantic(column.id, semantic);
      setSchema((prev) => (prev ? { ...prev, columns: res.columns } : prev));
      toast(t("columns_mapping_updated"), "success");
    } catch (err) {
      toast((err as ApiError)?.message ?? t("columns_update_failed"), "error");
    }
  };

  if (loading && !schema) return <Loading label={t("common_loading")} />;

  if (!schema || !schema.has_shape || schema.columns.length === 0) {
    return (
      <EmptyState
        icon="columns"
        title={t("columns_empty_title")}
        hint={t("columns_empty_hint")}
      />
    );
  }

  return (
    <div className="stack content-narrow">
      <div className="panel panel-pad">
        <div className="section-title">{t("columns_title")}</div>
        <p className="muted" style={{ marginTop: 0 }}>
          {t("columns_desc")}
        </p>
        <div className="table-wrap" style={{ maxHeight: "none" }}>
          <table className="grid" style={{ width: "100%" }}>
            <thead>
              <tr>
                <th style={{ minWidth: 220 }}>{t("columns_column")}</th>
                <th>{t("columns_detected_type")}</th>
                <th style={{ minWidth: 220 }}>{t("columns_meaning")}</th>
              </tr>
            </thead>
            <tbody>
              {schema.columns.map((col) => (
                <tr key={col.id} style={{ cursor: "default" }}>
                  <td title={col.header}>{col.header}</td>
                  <td className="faint">{columnRoleLabel(t, col.role)}</td>
                  <td>
                    <select
                      className="select"
                      value={col.semantic ?? ""}
                      disabled={!canEditData}
                      onChange={(e) => change(col, e.target.value)}
                    >
                      <option value="">{t("columns_auto")}</option>
                      {SEMANTICS.map((s) => (
                        <option key={s.value} value={s.value}>
                          {t(s.key)}
                        </option>
                      ))}
                    </select>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
