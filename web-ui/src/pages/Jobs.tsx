import { api, apiUrl } from "../api/client";
import type { ExportJobResult, Job } from "../api/types";
import { Icon } from "../components/Icon";
import { EmptyState, Progress } from "../components/ui";
import { useI18n } from "../lib/i18n";
import { formatBytes, formatInt } from "../lib/format";
import { jobKindLabel, jobPhaseLabel, jobTitleLabel } from "../lib/jobText";
import { useStore } from "../state/store";

const STATUS_COLOR: Record<Job["status"], string> = {
  queued: "var(--text-faint)",
  running: "var(--flame-orange)",
  succeeded: "var(--flame-amber)",
  failed: "var(--flame-red)",
  cancelled: "var(--text-faint)",
};

export function JobsPage() {
  const { t } = useI18n();
  const { jobs, refreshJobs, toast } = useStore();

  if (jobs.length === 0) {
    return <EmptyState icon="jobs" title={t("jobs_empty")} />;
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

  return (
    <div className="stack content-narrow">
      {jobs.map((job) => {
        const active = job.status === "running" || job.status === "queued";
        const result =
          job.kind === "export" ? (job.result as ExportJobResult | undefined) : undefined;
        return (
          <div key={job.id} className="panel panel-pad stack" style={{ gap: 8 }}>
            <div className="row wrap" style={{ justifyContent: "space-between" }}>
              <div className="row" style={{ gap: 10, minWidth: 0, flex: "1 1 280px" }}>
                <span
                  style={{
                    width: 8,
                    height: 8,
                    borderRadius: "50%",
                    background: STATUS_COLOR[job.status],
                  }}
                />
                <strong style={{ minWidth: 0, overflowWrap: "anywhere" }}>
                  {jobTitleLabel(t, job)}
                </strong>
                <span className="chip" style={{ flex: "none" }}>
                  {jobKindLabel(t, job.kind)}
                </span>
              </div>
              <div className="row" style={{ gap: 8, flex: "none" }}>
                <span className="faint">
                  {jobPhaseLabel(t, active ? job.progress.phase : "", job.status)}
                </span>
                {active && job.cancellable ? (
                  <button className="btn btn-sm btn-ghost" onClick={() => cancel(job.id)}>
                    {t("common_cancel")}
                  </button>
                ) : null}
              </div>
            </div>

            {active ? (
              <Progress percent={job.progress.total > 0 ? job.progress.percent : null} />
            ) : null}

            {job.message ? <div className="faint">{job.message}</div> : null}
            {job.error ? <div className="banner">{job.error}</div> : null}

            {result && job.status === "succeeded" ? (
              <div className="row" style={{ justifyContent: "space-between" }}>
                <span className="faint">
                  {formatInt(result.rows)} {t("common_rows")} · {formatBytes(result.bytes)}
                </span>
                <a className="btn btn-sm" href={apiUrl(result.download_url)}>
                  <Icon name="download" size={15} /> {t("common_download")}
                </a>
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
