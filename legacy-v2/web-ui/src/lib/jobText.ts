import type { Job, JobKind, JobStatus } from "../api/types";
import type { MessageKey, Translate } from "./i18n";

const KIND_KEYS: Record<JobKind, MessageKey> = {
  import: "jobs_kind_import",
  export: "jobs_kind_export",
  optimize: "jobs_kind_optimize",
  compact: "jobs_kind_compact",
  reindex: "jobs_kind_reindex",
  clear: "jobs_kind_clear",
  olap_build: "jobs_kind_olap_build",
};

const STATUS_KEYS: Record<JobStatus, MessageKey> = {
  queued: "jobs_status_queued",
  running: "jobs_status_running",
  succeeded: "jobs_status_succeeded",
  failed: "jobs_status_failed",
  cancelled: "jobs_status_cancelled",
};

const PHASE_KEYS: Record<string, MessageKey> = {
  reading: "jobs_phase_reading",
  inserting: "jobs_phase_inserting",
  indexing: "jobs_phase_indexing",
  writing: "jobs_phase_writing",
  optimizing: "jobs_phase_optimizing",
  compacting: "jobs_phase_compacting",
  "building projection": "jobs_phase_building_projection",
  clearing: "jobs_phase_clearing",
};

export function jobKindLabel(t: Translate, kind: JobKind): string {
  return t(KIND_KEYS[kind]);
}

export function jobStatusLabel(t: Translate, status: JobStatus): string {
  return t(STATUS_KEYS[status]);
}

export function jobPhaseLabel(t: Translate, phase: string, status: JobStatus): string {
  const normalized = phase.trim();
  if (!normalized) return jobStatusLabel(t, status);

  const match = /^([a-z ]+?)(\s+\(\d+\/\d+\))?$/.exec(normalized);
  if (!match) return normalized;
  const key = PHASE_KEYS[match[1]];
  if (!key) return normalized;
  return `${t(key)}${match[2] ?? ""}`;
}

export function jobTitleLabel(t: Translate, job: Job): string {
  if (job.kind === "import") {
    const multiple = /^Importing (\d+) files$/.exec(job.title);
    if (multiple) return t("jobs_importing_files", { count: multiple[1] });
    const single = /^Importing (.+)$/.exec(job.title);
    if (single) return t("jobs_importing_file", { file: single[1] });
  }

  if (job.kind === "export") {
    const match = /^Exporting (.+)$/.exec(job.title);
    if (match) return t("jobs_exporting_file", { file: match[1] });
  }

  return jobKindLabel(t, job.kind);
}
