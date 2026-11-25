import { useEffect, useState } from "react";
import { CheckCircle2, CircleX, LoaderCircle, X } from "lucide-react";
import { ErrorNote } from "./components";
import { count, messageOf, type Backend, type Job } from "./model";

type Props = {
  api: Backend;
  id: number;
  skipped: string[];
  changed: (job: Job) => void;
  dismiss: () => void;
};
const kind = { import: "Импорт", migration: "Перенос базы", export: "Экспорт" };
const state = {
  running: "выполняется",
  complete: "завершён",
  failed: "ошибка",
  cancelled: "отменён",
};
export function JobPanel({ api, id, changed, dismiss, skipped }: Props) {
  const [job, setJob] = useState<Job | null>(null);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const [cancelling, setCancelling] = useState(false);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const latest = await api.job(id);
        if (!active) return;
        setJob(latest);
        setError("");
        if (latest.state === "running") timer = setTimeout(poll, 400);
        else changed(latest);
      } catch (err) {
        if (active) setError(messageOf(err));
      }
    }
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [api, id, changed, retry]);
  async function cancel() {
    setCancelling(true);
    try {
      await api.cancel(id);
    } catch (err) {
      setError(messageOf(err));
      setCancelling(false);
    }
  }
  const pending = !job || job.state === "running";
  return (
    <section className="job-panel" aria-label="Ход операции">
      <header>
        <div>
          {pending ? (
            <LoaderCircle className="spin" size={18} />
          ) : job.state === "complete" ? (
            <CheckCircle2 size={18} />
          ) : (
            <CircleX size={18} />
          )}
          <strong role="status">
            {job
              ? kind[job.kind] + " · " + state[job.state]
              : "Получаем состояние операции…"}
          </strong>
        </div>
        {pending ? (
          <button disabled={cancelling} onClick={cancel}>
            {cancelling ? "Отменяем…" : "Отменить операцию"}
          </button>
        ) : (
          <button
            aria-label="Закрыть отчёт"
            className="icon-button"
            onClick={dismiss}
          >
            <X size={16} />
          </button>
        )}
      </header>
      {job?.message && <p>{job.message}</p>}
      {pending && job && (
        <>
          <progress
            aria-label="Прогресс операции"
            value={job.total && job.total > 0 ? job.processed : undefined}
            max={job.total || undefined}
          />
          <small>
            Обработано {count(job.processed)}
            {job.total != null && " из " + count(job.total)}
          </small>
        </>
      )}
      {error && (
        <ErrorNote retry={() => setRetry((r) => r + 1)}>{error}</ErrorNote>
      )}
      {skipped.length > 0 && (
        <div className="job-skipped">
          <strong>Пропущены до импорта</strong>
          <ul>
            {skipped.map((note, i) => (
              <li key={i}>{note}</li>
            ))}
          </ul>
        </div>
      )}
      {job?.result && (
        <div className="job-result">
          <p>
            Строк {count(job.result.rows)}
            {job.result.skippedSheets > 0 &&
              " · Пропущено листов " + count(job.result.skippedSheets)}
          </p>
          {job.result.path && <p className="path">{job.result.path}</p>}
          {job.result.files.length > 0 && (
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>Файл</th>
                    <th>Добавлено строк</th>
                    <th>Пропущено листов</th>
                    <th>Результат</th>
                  </tr>
                </thead>
                <tbody>
                  {job.result.files.map((f, i) => (
                    <tr key={i}>
                      <td>{f.fileName}</td>
                      <td>{count(f.insertedRows)}</td>
                      <td>{count(f.skippedSheets)}</td>
                      <td>
                        {f.error ||
                          (f.skippedSheets > 0 ? "Есть пропуски" : "Обработан")}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
