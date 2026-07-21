// Slide-in record card. Fetches the full record by id and shows schema fields
// and any extra source columns.

import { useEffect, useState } from "react";

import { api, ApiError } from "../api/client";
import type { RecordDto } from "../api/types";
import { Icon } from "./Icon";
import { Banner, Loading } from "./ui";
import { useI18n } from "../lib/i18n";
import { useStore } from "../state/store";

export function RecordDrawer({ id, onClose }: { id: number; onClose: () => void }) {
  const { t } = useI18n();
  const { openCompany } = useStore();
  const [record, setRecord] = useState<RecordDto | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setRecord(null);
    setError(null);
    api
      .record(id)
      .then((r) => alive && setRecord(r))
      .catch((err: ApiError) => alive && setError(err.message));
    return () => {
      alive = false;
    };
  }, [id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const nonEmpty = record?.fields.filter((f) => f.value.trim() !== "") ?? [];
  const companyField = nonEmpty.find(
    (f) => /edrpou|company code|компан/i.test(f.label) && f.value.trim() !== "",
  );

  return (
    <>
      <div className="backdrop" onClick={onClose} />
      <aside className="drawer" role="dialog" aria-label={t("record_title")}>
        <div className="drawer-head">
          <div className="grow">
            <div className="section-title" style={{ margin: 0 }}>
              {t("record_title")} #{id}
            </div>
          </div>
          {companyField ? (
            <button
              className="btn btn-sm"
              onClick={() => {
                openCompany(companyField.value.trim());
                onClose();
              }}
            >
              <Icon name="building" size={15} /> Company
            </button>
          ) : null}
          <button className="btn btn-ghost btn-sm" onClick={onClose} aria-label={t("common_close")}>
            <Icon name="close" />
          </button>
        </div>
        <div className="drawer-body">
          {error ? <Banner>{error}</Banner> : null}
          {!record && !error ? <Loading /> : null}
          {record ? (
            <>
              {nonEmpty.map((f, i) => (
                <div className="kv" key={`f-${i}`}>
                  <div className="kv-label">{f.label}</div>
                  <div className="kv-value">{f.value}</div>
                </div>
              ))}
              {record.extra.length > 0 ? (
                <>
                  <div className="section-title" style={{ marginTop: 18 }}>
                    {t("record_extra")}
                  </div>
                  {record.extra
                    .filter((f) => f.value.trim() !== "")
                    .map((f, i) => (
                      <div className="kv" key={`e-${i}`}>
                        <div className="kv-label">{f.label}</div>
                        <div className="kv-value">{f.value}</div>
                      </div>
                    ))}
                </>
              ) : null}
              <div className="kv" style={{ marginTop: 18 }}>
                <div className="kv-label">{t("record_source")}</div>
                <div className="kv-value faint">{record.source_file}</div>
              </div>
            </>
          ) : null}
        </div>
      </aside>
    </>
  );
}
