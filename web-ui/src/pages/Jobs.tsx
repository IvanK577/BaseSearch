import { api, apiUrl } from "../api/client";
import type { ExportJobResult } from "../api/types";
import { Icon } from "../components/Icon";
import { EmptyState, Progress } from "../components/ui";
import { useI18n } from "../lib/i18n";
import { formatBytes, formatInt } from "../lib/format";
import { jobKindLabel, jobPhaseLabel, jobTitleLabel } from "../lib/jobText";
import { navigate } from "../lib/router";
import { useStore } from "../state/store";

export function JobsPage() {
  const { t } = useI18n();
  const { jobs, refreshJobs, toast } = useStore();

  if (jobs.length === 0) {
    return (
      <div className="content-narrow jobs-page">
        <section className="panel">
          <EmptyState
            compact
            icon="jobs"
            title={t("jobs_empty")}
            action={
              <div className="row wrap">
                <button className="btn btn-sm" onClick={() => navigate("imports")}>
                  <Icon name="import" size={15} /> {t("nav_imports")}
                </button>
                <button className="btn btn-sm btn-ghost" onClick={() => navigate("exports")}>
                  <Icon name="export" size={15} /> {t("nav_exports")}
                </button>
              </div>
            }
          />
        </section>
      </div>
    );
  }

  const cancel = async (id: number) => {
    try {
      await api.cancelJob(id);
      toast(t("jobs_cancel_requested"), "info");
      refreshJobs();
    } catch {
      /* ignore */
    }
  };

  const activeCount = jobs.filter(
    (job) => job.status === "running" || job.status === "queued",
  ).length;
  const failedCount = jobs.filter((job) => job.status === "failed").length;

  return (
    <div className="stack content-narrow jobs-page">
      <div className="jobs-summary">
        <div className="row wrap">
          <strong>{formatInt(jobs.length)}</strong>
          <span className="faint">{t("jobs_title")}</span>
          {activeCount > 0 ? <span className="status-count running">{activeCount}</span> : null}
          {failedCount > 0 ? <span className="status-count failed">{failedCount}</span> : null}
        </div>
        <button
          className="icon-button"
          onClick={refreshJobs}
          aria-label={t("jobs_refresh")}
          title={t("jobs_refresh")}
        >
          <Icon name="refresh" size={15} />
        </button>
      </div>
      <section className="panel jobs-list">
      {jobs.map((job) => {
        const active = job.status === "running" || job.status === "queued";
        const result =
          job.kind === "export" ? (job.result as ExportJobResult | undefined) : undefined;
        return (
          <article key={job.id} className="job-row" data-status={job.status}>
            <div className="job-row-head">
              <span className="job-status-dot" aria-hidden="true" />
              <div className="job-title">
                <strong>
                  {jobTitleLabel(t, job)}
                </strong>
                <span className="job-kind">
                  {jobKindLabel(t, job.kind)}
                </span>
              </div>
              <span className="job-phase faint">
                {jobPhaseLabel(t, active ? job.progress.phase : "", job.status)}
              </span>
              {active && job.cancellable ? (
                <button className="btn btn-sm btn-ghost" onClick={() => cancel(job.id)}>
                  {t("common_cancel")}
                </button>
              ) : null}
            </div>

            {active ? (
              <Progress percent={job.progress.total > 0 ? job.progress.percent : null} />
            ) : null}

            {job.message ? <div className="faint">{job.message}</div> : null}
            {job.error ? <div className="banner">{job.error}</div> : null}

            {result && job.status === "succeeded" ? (
              <div className="job-result-row">
                <span className="faint">
                  {formatInt(result.rows)} {t("common_rows")} · {formatBytes(result.bytes)}
                </span>
                <a className="btn btn-sm" href={apiUrl(result.download_url)}>
                  <Icon name="download" size={15} /> {t("common_download")}
                </a>
              </div>
            ) : null}
          </article>
        );
      })}
      </section>
    </div>
  );
}
