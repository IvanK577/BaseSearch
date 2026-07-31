import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, ApiError } from "../api/client";
import type {
  ImportJobResult,
  ImportLogEntry,
  Job,
  FixedSemanticField,
  SemanticField,
  SheetPeek,
  SourceMappingProfile,
  SourceMappingProfileCollection,
  WorkbookPeek,
} from "../api/types";
import { Icon } from "../components/Icon";
import { Progress, Spinner } from "../components/ui";
import type { MessageKey, Translate } from "../lib/i18n";
import { useI18n } from "../lib/i18n";
import { formatInt, formatPercent } from "../lib/format";
import { jobPhaseLabel, jobTitleLabel } from "../lib/jobText";
import {
  columnRoleLabel,
  layoutLabel,
  semanticLabel,
  SEMANTICS,
} from "../lib/semantics";
import { useStore } from "../state/store";
import {
  effectiveFixedValue,
  effectiveMapping,
  mappingSelection,
  type FixedValueOverrides,
} from "../lib/sourceProfiles";

const PROFILE_ERROR_KEYS: Partial<Record<string, MessageKey>> = {
  profile_signature_mismatch: "imports_profile_signature_mismatch",
  profile_name_conflict: "imports_profile_name_conflict",
  corrupt_profile: "imports_profile_corrupt",
};

function importErrorMessage(
  error: unknown,
  t: Translate,
  fallbackKey: MessageKey,
): string {
  if (!(error instanceof ApiError)) return t(fallbackKey);
  const localizedKey = PROFILE_ERROR_KEYS[error.code];
  if (localizedKey) return t(localizedKey);
  if (error.code === "network" || error.code === "internal") return t(fallbackKey);
  return error.message.trim() || t(fallbackKey);
}

export function ImportsPage() {
  const { t } = useI18n();
  const { jobs, refreshJobs, toast, canEditData } = useStore();
  const [drag, setDrag] = useState(false);
  const [log, setLog] = useState<ImportLogEntry[]>([]);
  const [logError, setLogError] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const peekRef = useRef<HTMLInputElement>(null);
  const [peek, setPeek] = useState<{
    name: string;
    file: File;
    data: WorkbookPeek;
    selectedSheets: string[];
    semanticOverrides: Record<string, Record<number, SemanticField | null>>;
    selectedProfileIds: Record<string, number>;
    fixedValueOverrides: Record<string, FixedValueOverrides>;
    profileNames: Record<string, string>;
  } | null>(null);
  const [batchFiles, setBatchFiles] = useState<File[]>([]);
  const [peeking, setPeeking] = useState(false);
  const [profiles, setProfiles] = useState<SourceMappingProfileCollection>({
    profiles: [],
    ignored_corrupt_rows: [],
  });

  const refreshProfiles = useCallback(() => {
    api.mappingProfiles().then(setProfiles).catch(() => {});
  }, []);

  const importJobs = useMemo(
    () => jobs.filter((j) => j.kind === "import"),
    [jobs],
  );
  const visibleImportJobs = useMemo(
    () =>
      importJobs
        .filter((job) =>
          job.status === "queued" || job.status === "running" || job.status === "failed",
        )
        .slice(0, 4),
    [importJobs],
  );
  const signature = importJobs.map((j) => `${j.id}:${j.status}`).join(",");

  useEffect(() => {
    api
      .importLog(50)
      .then((entries) => {
        setLog(entries);
        setLogError(null);
      })
      .catch((error) =>
        setLogError((error as ApiError)?.message ?? t("imports_history_unavailable")),
      );
  }, [signature]);

  useEffect(() => {
    refreshProfiles();
  }, [refreshProfiles]);

  const upload = async (
    files: FileList | File[],
    selectedSheets?: string[],
    sheetSemantics?: Record<string, Record<number, SemanticField | null>>,
    sheetProfiles?: Record<string, number>,
    sheetFixedValues?: Record<string, FixedValueOverrides>,
    peekToken?: string,
  ) => {
    if (!files || Array.from(files).length === 0) return;
    const send = (token?: string) =>
      api.uploadImport(
        files,
        selectedSheets,
        sheetSemantics,
        sheetProfiles,
        sheetFixedValues,
        token,
      );
    try {
      try {
        await send(peekToken);
      } catch (err) {
        // The retained preview was swept or the server restarted: fall back to
        // sending the bytes. Any other failure is a real error.
        if (!peekToken || (err as ApiError)?.code !== "preview_expired") throw err;
        await send(undefined);
      }
      toast(t("imports_started"), "info");
      setPeek(null);
      setBatchFiles([]);
      refreshJobs();
    } catch (err) {
      toast(importErrorMessage(err, t, "imports_upload_failed"), "error");
    }
  };

  const preview = async (file: File | undefined) => {
    if (!file) return;
    setPeeking(true);
    try {
      const data = await api.peekImport(file);
      setPeek({
        name: file.name,
        file,
        data,
        selectedSheets: data.sheets.map((sheet) => sheet.name),
        semanticOverrides: {},
        selectedProfileIds: {},
        fixedValueOverrides: {},
        profileNames: {},
      });
      setBatchFiles([]);
    } catch (err) {
      toast(importErrorMessage(err, t, "imports_preview_failed"), "error");
    } finally {
      setPeeking(false);
    }
  };

  const chooseFiles = (files: FileList | File[]) => {
    const chosen = Array.from(files);
    if (chosen.length === 0) return;
    if (chosen.length === 1) {
      preview(chosen[0]);
      return;
    }
    setPeek(null);
    setBatchFiles(chosen);
  };

  const toggleSheet = (sheetName: string) => {
    setPeek((current) => {
      if (!current) return current;
      const selected = new Set(current.selectedSheets);
      if (selected.has(sheetName)) selected.delete(sheetName);
      else selected.add(sheetName);
      return { ...current, selectedSheets: Array.from(selected) };
    });
  };

  const setSemantic = (sheetName: string, column: number, value: string) => {
    setPeek((current) => {
      if (!current) return current;
      const sheet = { ...(current.semanticOverrides[sheetName] ?? {}) };
      if (value === "__auto") delete sheet[column];
      else sheet[column] = value === "__none" ? null : (value as SemanticField);
      const semanticOverrides = { ...current.semanticOverrides };
      if (Object.keys(sheet).length === 0) delete semanticOverrides[sheetName];
      else semanticOverrides[sheetName] = sheet;
      return { ...current, semanticOverrides };
    });
  };

  const profileForSheet = (
    current: NonNullable<typeof peek>,
    sheet: SheetPeek,
  ): SourceMappingProfile | null => {
    const id = current.selectedProfileIds[sheet.name];
    if (!id) return null;
    return (
      sheet.profile_suggestions.profiles.find((profile) => profile.id === id) ??
      profiles.profiles.find((profile) => profile.id === id) ??
      null
    );
  };

  const selectProfile = (sheetName: string, profileId: number | null) => {
    setPeek((current) => {
      if (!current) return current;
      const selectedProfileIds = { ...current.selectedProfileIds };
      if (profileId === null) delete selectedProfileIds[sheetName];
      else selectedProfileIds[sheetName] = profileId;
      return { ...current, selectedProfileIds };
    });
  };

  const setFixedValue = (
    sheetName: string,
    semantic: FixedSemanticField,
    value: string,
  ) => {
    setPeek((current) => {
      if (!current) return current;
      const sheetValues = { ...(current.fixedValueOverrides[sheetName] ?? {}) };
      if (value.trim()) sheetValues[semantic] = value;
      else delete sheetValues[semantic];
      const fixedValueOverrides = { ...current.fixedValueOverrides };
      if (Object.keys(sheetValues).length === 0) delete fixedValueOverrides[sheetName];
      else fixedValueOverrides[sheetName] = sheetValues;
      return { ...current, fixedValueOverrides };
    });
  };

  const setProfileName = (sheetName: string, name: string) => {
    setPeek((current) =>
      current
        ? {
            ...current,
            profileNames: { ...current.profileNames, [sheetName]: name },
          }
        : current,
    );
  };

  const saveProfile = async (sheet: SheetPeek) => {
    if (!peek) return;
    const name = (peek.profileNames[sheet.name] ?? "").trim();
    if (!name) {
      toast(t("imports_profile_name_required"), "error");
      return;
    }
    const selectedProfile = profileForSheet(peek, sheet);
    const fixedValues = (["Currency", "WeightUnit"] as FixedSemanticField[])
      .map((semantic) => [
        semantic,
        effectiveFixedValue(
          semantic,
          peek.fixedValueOverrides[sheet.name] ?? {},
          selectedProfile,
        ).trim(),
      ] as const)
      .filter(([, value]) => value)
      .reduce<FixedValueOverrides>((values, [semantic, value]) => {
        values[semantic] = value;
        return values;
      }, {});
    try {
      const saved = await api.saveMappingProfile({
        name,
        signature: sheet.signature,
        mapping: effectiveMapping(
          sheet.columns,
          peek.semanticOverrides[sheet.name] ?? {},
          selectedProfile,
        ),
        fixed_values: fixedValues,
      });
      setProfiles((current) => ({
        ...current,
        profiles: [saved, ...current.profiles.filter((profile) => profile.id !== saved.id)],
      }));
      setPeek((current) => {
        if (!current) return current;
        return {
          ...current,
          selectedProfileIds: {
            ...current.selectedProfileIds,
            [sheet.name]: saved.id,
          },
          profileNames: { ...current.profileNames, [sheet.name]: "" },
          data: {
            ...current.data,
            sheets: current.data.sheets.map((candidate) =>
              candidate.name === sheet.name
                ? {
                    ...candidate,
                    profile_suggestions: {
                      ...candidate.profile_suggestions,
                      profiles: [
                        saved,
                        ...candidate.profile_suggestions.profiles.filter(
                          (profile) => profile.id !== saved.id,
                        ),
                      ],
                    },
                  }
                : candidate,
            ),
          },
        };
      });
      toast(t("imports_profile_saved"), "success");
    } catch (err) {
      toast(importErrorMessage(err, t, "imports_profile_save_failed"), "error");
    }
  };

  const deleteProfile = async (profile: SourceMappingProfile) => {
    try {
      await api.deleteMappingProfile(profile.id);
      setProfiles((current) => ({
        ...current,
        profiles: current.profiles.filter((candidate) => candidate.id !== profile.id),
      }));
      setPeek((current) => {
        if (!current) return current;
        const selectedProfileIds = Object.fromEntries(
          Object.entries(current.selectedProfileIds).filter(([, id]) => id !== profile.id),
        );
        return {
          ...current,
          selectedProfileIds,
          data: {
            ...current.data,
            sheets: current.data.sheets.map((sheet) => ({
              ...sheet,
              profile_suggestions: {
                ...sheet.profile_suggestions,
                profiles: sheet.profile_suggestions.profiles.filter(
                  (candidate) => candidate.id !== profile.id,
                ),
              },
            })),
          },
        };
      });
      toast(t("imports_profile_deleted"), "success");
    } catch (err) {
      toast(importErrorMessage(err, t, "imports_profile_delete_failed"), "error");
    }
  };

  const uploadPreview = () => {
    if (!peek) return;
    const selected = new Set(peek.selectedSheets);
    const sheetProfiles = Object.fromEntries(
      Object.entries(peek.selectedProfileIds).filter(([sheet]) => selected.has(sheet)),
    );
    const sheetFixedValues: Record<string, FixedValueOverrides> = {};
    for (const sheet of peek.data.sheets.filter((candidate) => selected.has(candidate.name))) {
      const profile = profileForSheet(peek, sheet);
      const values = (["Currency", "WeightUnit"] as FixedSemanticField[]).reduce<
        FixedValueOverrides
      >((result, semantic) => {
        const value = effectiveFixedValue(
          semantic,
          peek.fixedValueOverrides[sheet.name] ?? {},
          profile,
        ).trim();
        if (value) result[semantic] = value;
        return result;
      }, {});
      if (Object.keys(values).length > 0) sheetFixedValues[sheet.name] = values;
    }
    upload(
      [peek.file],
      peek.selectedSheets,
      peek.semanticOverrides,
      sheetProfiles,
      sheetFixedValues,
      // The preview already put this exact file on the server; claim it rather
      // than uploading every byte a second time.
      peek.data.token,
    );
  };

  return (
    <div className="stack content-narrow imports-page">
      {!canEditData ? (
        <div className="banner banner-warn">
          {t("imports_editor_required")}
        </div>
      ) : (
        <section className="panel import-intake">
          <div
            className={`dropzone ${drag ? "drag" : ""}`}
            role="button"
            tabIndex={0}
            onDragOver={(event) => {
              event.preventDefault();
              setDrag(true);
            }}
            onDragLeave={() => setDrag(false)}
            onDrop={(event) => {
              event.preventDefault();
              setDrag(false);
              chooseFiles(event.dataTransfer.files);
            }}
            onClick={() => fileRef.current?.click()}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                fileRef.current?.click();
              }
            }}
          >
            <Icon name="import" size={26} className="empty-icon" />
            <div className="dropzone-copy">
              <strong>{t("imports_drop")}</strong>
              <span>{t("imports_hint")}</span>
            </div>
            <input
              ref={fileRef}
              type="file"
              multiple
              accept=".xlsx,.xlsb,.xls,.xlsm,.ods,.csv,.tsv"
              onChange={(event) => {
                if (event.target.files) chooseFiles(event.target.files);
                event.target.value = "";
              }}
            />
          </div>
          <div className="import-intake-actions">
            <span className="faint">{t("imports_preview_hint")}</span>
            <button
              className="btn btn-sm"
              onClick={() => peekRef.current?.click()}
              disabled={peeking}
            >
              {peeking ? <Spinner /> : <Icon name="search" size={14} />} {t("imports_preview")}
            </button>
            <input
              ref={peekRef}
              type="file"
              accept=".xlsx,.xlsb,.xls,.xlsm,.ods,.csv,.tsv"
              onChange={(event) => {
                preview(event.target.files?.[0]);
                event.target.value = "";
              }}
            />
          </div>
        </section>
      )}

      {batchFiles.length > 1 ? (
        <div className="panel panel-pad stack" style={{ gap: 12 }}>
          <div className="row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
            <div>
              <div className="section-title" style={{ margin: 0 }}>
                {t("imports_files_selected", { count: formatInt(batchFiles.length) })}
              </div>
              <div className="faint" style={{ marginTop: 4 }}>
                {batchFiles.slice(0, 4).map((file) => file.name).join(" · ")}
                {batchFiles.length > 4 ? ` · +${batchFiles.length - 4}` : ""}
              </div>
            </div>
            <div className="row" style={{ gap: 8 }}>
              <button className="btn btn-sm btn-primary" onClick={() => upload(batchFiles)}>
                <Icon name="import" size={14} /> {t("imports_import_files")}
              </button>
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => setBatchFiles([])}
                aria-label={t("common_close")}
                title={t("common_close")}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {canEditData &&
      (profiles.profiles.length > 0 || profiles.ignored_corrupt_rows.length > 0) ? (
        <details className="panel source-profile-manager">
          <summary>
            <span className="row" style={{ gap: 8 }}>
              <Icon name="columns" size={14} />
              {t("imports_saved_profiles", { count: profiles.profiles.length })}
            </span>
          </summary>
          <div className="source-profile-list">
            {profiles.profiles.map((profile) => (
              <div className="source-profile-list-row" key={profile.id}>
                <div style={{ minWidth: 0 }}>
                  <div className="source-profile-name">{profile.name}</div>
                  <div className="faint">
                    {t("imports_profile_columns", { count: profile.mapping.length })}
                    {profile.fixed_values.Currency
                      ? ` · ${profile.fixed_values.Currency}`
                      : ""}
                    {profile.fixed_values.WeightUnit
                      ? ` · ${profile.fixed_values.WeightUnit}`
                      : ""}
                  </div>
                </div>
                <button
                  className="btn btn-sm btn-ghost"
                  onClick={() => deleteProfile(profile)}
                  aria-label={t("imports_profile_delete", { name: profile.name })}
                  title={t("imports_profile_delete", { name: profile.name })}
                >
                  <Icon name="trash" size={14} />
                </button>
              </div>
            ))}
            {profiles.ignored_corrupt_rows.length > 0 ? (
              <div className="banner banner-warn">
                {t("imports_profiles_corrupt", {
                  count: profiles.ignored_corrupt_rows.length,
                })}
              </div>
            ) : null}
          </div>
        </details>
      ) : null}

      {peek ? (
        <div className="panel panel-pad stack" style={{ gap: 12 }}>
          <div className="row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
            <div className="section-title" style={{ margin: 0 }} title={peek.name}>
              {peek.name}
            </div>
            <div className="row" style={{ gap: 8 }}>
              <button
                className="btn btn-sm btn-primary"
                disabled={peek.selectedSheets.length === 0}
                onClick={uploadPreview}
              >
                <Icon name="import" size={14} /> {t("imports_import_selected")}
              </button>
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => setPeek(null)}
                aria-label={t("common_close")}
                title={t("common_close")}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
          </div>
          {peek.data.sheets.map((sheet) => {
            const selectedProfile = profileForSheet(peek, sheet);
            const suggestions = sheet.profile_suggestions.profiles;
            const overrides = peek.semanticOverrides[sheet.name] ?? {};
            return (
            <div key={sheet.name} className="stack source-sheet" style={{ gap: 8 }}>
              <label className="check-row" style={{ justifyContent: "flex-start" }}>
                <input
                  type="checkbox"
                  checked={peek.selectedSheets.includes(sheet.name)}
                  onChange={() => toggleSheet(sheet.name)}
                />
                <span>
                  {sheet.name} · {formatInt(sheet.rows)} {t("imports_preview_rows")} ×{" "}
                  {formatInt(sheet.cols)} {t("imports_preview_cols")} ·{" "}
                  {t("imports_header_row", { row: sheet.header_row })} ·{" "}
                  {t("imports_layout_label", { layout: layoutLabel(t, sheet.layout) })}
                </span>
              </label>
              <div className="source-profile-strip">
                <div style={{ minWidth: 0 }}>
                  <div className="source-profile-label">
                    {selectedProfile
                      ? t("imports_profile_applied", { name: selectedProfile.name })
                      : suggestions.length > 0
                        ? t("imports_profile_exact_found")
                        : t("imports_profile_no_exact")}
                  </div>
                  <div className="faint">{t("imports_profile_exact_hint")}</div>
                </div>
                <div className="row wrap" style={{ gap: 8 }}>
                  {suggestions.length > 0 ? (
                    <select
                      className="select input-compact"
                      aria-label={t("imports_profile_choose")}
                      value={selectedProfile?.id ?? ""}
                      onChange={(event) =>
                        selectProfile(
                          sheet.name,
                          event.target.value ? Number(event.target.value) : null,
                        )
                      }
                    >
                      <option value="">{t("imports_profile_choose")}</option>
                      {suggestions.map((profile) => (
                        <option key={profile.id} value={profile.id}>
                          {profile.name}
                        </option>
                      ))}
                    </select>
                  ) : null}
                  {selectedProfile ? (
                    <button
                      className="btn btn-sm btn-ghost"
                      onClick={() => selectProfile(sheet.name, null)}
                    >
                      {t("imports_profile_ignore")}
                    </button>
                  ) : null}
                </div>
              </div>
              <details className="source-profile-options">
                <summary>{t("imports_profile_options")}</summary>
                <div className="source-profile-options-grid">
                  <label>
                    <span className="field-label">{t("imports_profile_currency")}</span>
                    <input
                      className="input"
                      maxLength={32}
                      value={effectiveFixedValue(
                        "Currency",
                        peek.fixedValueOverrides[sheet.name] ?? {},
                        selectedProfile,
                      )}
                      placeholder={t("imports_profile_currency_hint")}
                      onChange={(event) =>
                        setFixedValue(sheet.name, "Currency", event.target.value)
                      }
                    />
                  </label>
                  <label>
                    <span className="field-label">{t("imports_profile_weight_unit")}</span>
                    <input
                      className="input"
                      maxLength={32}
                      value={effectiveFixedValue(
                        "WeightUnit",
                        peek.fixedValueOverrides[sheet.name] ?? {},
                        selectedProfile,
                      )}
                      placeholder={t("imports_profile_weight_unit_hint")}
                      onChange={(event) =>
                        setFixedValue(sheet.name, "WeightUnit", event.target.value)
                      }
                    />
                  </label>
                  <label className="source-profile-save-field">
                    <span className="field-label">{t("imports_profile_name")}</span>
                    <span className="row" style={{ gap: 8 }}>
                      <input
                        className="input"
                        maxLength={100}
                        value={peek.profileNames[sheet.name] ?? ""}
                        placeholder={t("imports_profile_name_hint")}
                        onChange={(event) => setProfileName(sheet.name, event.target.value)}
                      />
                      <button
                        className="btn btn-sm"
                        onClick={() => saveProfile(sheet)}
                      >
                        {t("imports_profile_save")}
                      </button>
                    </span>
                  </label>
                </div>
              </details>
              <div className="table-wrap" style={{ maxHeight: 320 }}>
                <table className="grid" style={{ width: "100%" }}>
                  <thead>
                    <tr>
                      <th style={{ width: 40 }}>#</th>
                      <th style={{ minWidth: 160 }}>{t("imports_preview_column")}</th>
                      <th style={{ minWidth: 140 }}>{t("imports_detected_as")}</th>
                      <th>{t("imports_preview_sample")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {sheet.columns.map((col) => (
                      <tr key={col.index} style={{ cursor: "default" }}>
                        <td className="faint">{col.index + 1}</td>
                        <td title={col.header}>{col.header || "—"}</td>
                        <td>
                          <select
                            className="select input-compact"
                            value={
                              Object.prototype.hasOwnProperty.call(
                                overrides,
                                col.index,
                              )
                                ? (overrides[col.index] ?? "__none")
                                : "__auto"
                            }
                            onChange={(event) =>
                              setSemantic(sheet.name, col.index, event.target.value)
                            }
                          >
                            <option value="__auto">
                              {selectedProfile
                                ? t("imports_profile_inherited", {
                                    meaning:
                                      mappingSelection(
                                        col.index,
                                        {},
                                        selectedProfile,
                                      ) === null
                                        ? t("imports_no_meaning")
                                        : semanticLabel(
                                            t,
                                            mappingSelection(
                                              col.index,
                                              {},
                                              selectedProfile,
                                            ) as SemanticField,
                                          ),
                                  })
                                : t("imports_auto_detected", {
                                    meaning: col.semantic
                                      ? semanticLabel(t, col.semantic)
                                      : columnRoleLabel(t, col.role),
                                  })}
                            </option>
                            <option value="__none">{t("imports_no_meaning")}</option>
                            {SEMANTICS.map((semantic) => (
                              <option key={semantic.value} value={semantic.value}>
                                {t(semantic.key)}
                              </option>
                            ))}
                          </select>
                        </td>
                        <td className="faint" title={col.sample}>{col.sample || "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
            );
          })}
        </div>
      ) : null}

      {visibleImportJobs.length > 0 ? (
        <section className="panel panel-pad stack import-activity" style={{ gap: 10 }}>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <div className="section-title" style={{ margin: 0 }}>
              {t("nav_jobs")}
            </div>
            <a className="icon-button" href="#/jobs" aria-label={t("nav_jobs")} title={t("nav_jobs")}>
              <Icon name="arrow-right" size={15} />
            </a>
          </div>
          {visibleImportJobs.map((job) => (
            <ImportJobRow key={job.id} job={job} />
          ))}
        </section>
      ) : null}

      <section className="panel panel-pad import-history">
        <div className="section-title">{t("imports_history")}</div>
        {logError ? (
          <div className="banner">{logError}</div>
        ) : log.length === 0 ? (
          <div className="import-history-empty">
            <Icon name="database" size={20} />
            <div>
              <strong>{t("common_none")}</strong>
              <span>{t("imports_hint")}</span>
            </div>
          </div>
        ) : (
          <div className="table-wrap" style={{ maxHeight: "none" }}>
            <table className="grid" style={{ width: "100%" }}>
              <thead>
                <tr>
                  <th style={{ minWidth: 200 }}>{t("imports_file")}</th>
                  <th>{t("imports_imported")}</th>
                  <th>{t("imports_duplicates")}</th>
                  <th>{t("common_rows")}</th>
                  <th>{t("imports_filled")}</th>
                  <th>{t("imports_layout")}</th>
                  <th>{t("imports_when")}</th>
                </tr>
              </thead>
              <tbody>
                {log.map((entry, i) => (
                  <tr key={`${entry.file_name}-${i}`} style={{ cursor: "default" }}>
                    <td title={entry.file_name}>{entry.file_name}</td>
                    <td>{formatInt(entry.imported)}</td>
                    <td>{formatInt(entry.duplicates)}</td>
                    <td>{formatInt(entry.total_rows)}</td>
                    <td>{formatPercent(entry.quality.filled_percent, 0)}</td>
                    <td className="faint" title={entry.quality.warnings.join("\n")}>
                      {entry.quality.layout ? layoutLabel(t, entry.quality.layout) : "—"}
                      {entry.quality.warnings.length > 0
                        ? ` · ${t("imports_warnings", {
                            count: entry.quality.warnings.length,
                          })}`
                        : ""}
                    </td>
                    <td className="faint">{entry.imported_at}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}

function ImportJobRow({ job }: { job: Job }) {
  const { t } = useI18n();
  const active = job.status === "running" || job.status === "queued";
  const result = job.result as ImportJobResult | undefined;
  return (
    <div className="import-job-row" data-status={job.status}>
      <div className="import-job-head">
        <span className="job-status-dot" aria-hidden="true" />
        <strong>{jobTitleLabel(t, job)}</strong>
        <span className="faint">
          {jobPhaseLabel(t, active ? job.progress.phase : "", job.status)}
        </span>
      </div>
      {active ? (
        <Progress percent={job.progress.total > 0 ? job.progress.percent : null} />
      ) : null}
      {job.message ? <div className="faint">{job.message}</div> : null}
      {job.error ? <div className="banner">{job.error}</div> : null}
      {result && !active ? (
        <div className="import-job-result faint">
          {result.files.map((f, i) => (
            <div key={i}>
              {f.error
                ? `${f.file_name}: ${f.error}`
                : f.skipped_duplicate_of
                  ? t("imports_already_imported", { file: f.file_name })
                  : t("imports_result_summary", {
                      file: f.file_name,
                      rows: formatInt(f.imported),
                      duplicates: formatInt(f.duplicates),
                    })}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
