import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowUpRight,
  ChartNoAxesColumnIncreasing,
  ChevronRight,
  Database,
  Folder,
  HardDrive,
  Plus,
  Search,
  Table2,
} from "lucide-react";
import { desktop } from "./backend";
import { Empty, ErrorNote, Modal } from "./components";
import { DataView, OpenData, Welcome } from "./DataView";
import { ImportWizard } from "./ImportWizard";
import { JobPanel } from "./JobPanel";
import { SearchView } from "./SearchView";
import { SummaryView } from "./SummaryView";
import {
  count,
  emptyQuery,
  messageOf,
  type Backend,
  type Job,
  type Query,
  type Workspace,
} from "./model";

const views = {
  search: {
    title: "Поиск",
    sub: "Поиск и фильтрация записей по всей базе.",
    icon: Search,
  },
  data: {
    title: "Данные",
    sub: "Файлы и листы в вашей локальной базе.",
    icon: Table2,
  },
  summary: {
    title: "Сводка",
    sub: "Показатели и распределение записей по текущему запросу.",
    icon: ChartNoAxesColumnIncreasing,
  },
};
type View = keyof typeof views;

export function App({ api = desktop }: { api?: Backend }) {
  const [view, setView] = useState<View>("data");
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [query, setQuery] = useState<Query>(emptyQuery);
  const [error, setError] = useState("");
  const [paths, setPaths] = useState<string[] | null>(null);
  const [legacy, setLegacy] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const [jobId, setJobId] = useState<number | null>(null);
  const [skipped, setSkipped] = useState<string[]>([]);
  const [running, setRunning] = useState(false);
  const last = useRef<Workspace | null>(null);
  const sequence = useRef(0);
  const initialized = useRef(false);
  const load = useCallback(async () => {
    const request = ++sequence.current;
    try {
      const info = await api.workspace();
      if (request !== sequence.current) return;
      if (!initialized.current) {
        initialized.current = true;
        setView(info.rowCount ? "search" : "data");
      }
      if (!last.current || (last.current.rowCount === 0 && info.rowCount > 0))
        setQuery((prev) => ({ ...prev, revision: info.revision }));
      last.current = info;
      setWorkspace(info);
      setError("");
      return info;
    } catch (err) {
      if (request === sequence.current) setError(messageOf(err));
    }
  }, [api]);
  useEffect(() => {
    void load();
    return () => {
      sequence.current++;
    };
  }, [load]);
  const complete = useCallback(
    (job: Job) => {
      setRunning(false);
      if (job.kind === "export") return;
      void load().then((info) => {
        if (job.state !== "complete" || !job.result?.rows || !info) return;
        setQuery((prev) => ({ ...prev, revision: info.revision }));
        setView("search");
      });
    },
    [load],
  );
  const busy = running || picking;
  function start(id: number, skippedFiles: string[] = []) {
    setSkipped(skippedFiles);
    setJobId(id);
    setRunning(true);
    setPaths(null);
    setLegacy(null);
    setError("");
  }
  async function pick(migration = false) {
    if (busy) return;
    setPicking(true);
    setError("");
    try {
      if (migration) {
        const path = await api.chooseLegacy();
        if (path) setLegacy(path);
      } else {
        const files = await api.chooseFiles();
        if (files.length) setPaths(Array.from(new Set(files)));
      }
    } catch (err) {
      setError(messageOf(err));
    } finally {
      setPicking(false);
    }
  }
  async function migrate() {
    if (!legacy || busy) return;
    setPicking(true);
    setError("");
    try {
      start(await api.migrate(legacy));
    } catch (err) {
      setError(messageOf(err));
      setLegacy(null);
    } finally {
      setPicking(false);
    }
  }
  const first = workspace?.rowCount === 0 && workspace.sources.length === 0;
  const openData = () => setView("data");
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
          <span>Base Search</span>
        </div>
        <nav aria-label="Разделы приложения">
          {(Object.keys(views) as View[]).map((v) => {
            const Icon = views[v].icon;
            return (
              <button
                key={v}
                aria-label={views[v].title}
                aria-current={view === v ? "page" : undefined}
                onClick={() => setView(v)}
              >
                <Icon size={19} strokeWidth={1.7} />
                <span>{views[v].title}</span>
                {view === v && <i />}
              </button>
            );
          })}
        </nav>
        <div className="sidebar-bottom">
          <div className="base-label">
            <Database size={16} />
            <span>Локальная база</span>
          </div>
          <div className="side-stats">
            <span>Строки</span>
            <b>{workspace ? count(workspace.rowCount) : "—"}</b>
            <span>Источники</span>
            <b>{workspace ? count(workspace.sources.length) : "—"}</b>
          </div>
          <div className="version">
            <span>Base Search</span>
            <span>v1.0.0</span>
          </div>
        </div>
      </aside>
      <div className="main-area">
        <header className="topbar">
          <div>
            <Folder size={15} />
            <span>Рабочая область</span>
            <ChevronRight size={13} />
            <b>{views[view].title}</b>
          </div>
          <span className="local-status">
            <i />
            Локальный режим
          </span>
        </header>
        <main className="content">
          <div className="page-head">
            <div>
              <h1>{views[view].title}</h1>
              <p>{views[view].sub}</p>
            </div>
            {workspace && !first && view === "data" && (
              <button
                className="primary"
                disabled={busy}
                onClick={() => pick()}
              >
                <Plus size={16} />
                Добавить файлы
              </button>
            )}
          </div>
          {error && <ErrorNote retry={load}>{error}</ErrorNote>}
          {jobId !== null && (
            <JobPanel
              key={jobId}
              id={jobId}
              skipped={skipped}
              api={api}
              changed={complete}
              dismiss={() => setJobId(null)}
            />
          )}
          {!workspace ? (
            !error && <p role="status">Открываем локальную базу…</p>
          ) : view === "data" ? (
            <>
              {first && (
                <Welcome
                  busy={busy}
                  add={() => pick()}
                  migrate={() => pick(true)}
                />
              )}
              <DataView workspace={workspace} />
              <div className="format-note">
                <HardDrive size={15} />
                <span>Поддерживаемые форматы</span>
                <span className="format-types">
                  XLSX · XLS · XLSB · XLSM · ODS · CSV · TSV
                </span>
              </div>
            </>
          ) : first ? (
            <Empty
              kind={view === "search" ? "search" : "data"}
              title="База пока пуста"
            >
              <p>
                Добавьте таблицу или перенесите данные из Base Search 2.2.0.
              </p>
              <OpenData open={openData} />
            </Empty>
          ) : view === "search" ? (
            <SearchView
              api={api}
              workspace={workspace}
              query={query}
              apply={setQuery}
              jobStarted={start}
              busy={busy}
              reload={load}
            />
          ) : (
            <SummaryView
              api={api}
              workspace={workspace}
              query={query}
              refresh={() => {
                void load();
                setQuery({ ...query, revision: workspace.revision });
              }}
              drill={(filter) => {
                setQuery({ ...query, filters: [...query.filters, filter] });
                setView("search");
              }}
            />
          )}
        </main>
      </div>
      {paths && (
        <ImportWizard
          paths={paths}
          api={api}
          close={() => setPaths(null)}
          started={start}
        />
      )}
      {legacy && (
        <Modal
          title="Перенести базу 2.2.0"
          close={() => {
            if (!picking) setLegacy(null);
          }}
        >
          <div className="modal-content">
            <p>
              Записи будут скопированы в текущую пустую базу. Исходный файл
              сохранится.
            </p>
            <p className="path">{legacy}</p>
          </div>
          <footer className="modal-foot">
            <button disabled={picking} onClick={() => setLegacy(null)}>
              Отмена
            </button>
            <button className="primary" disabled={picking} onClick={migrate}>
              Начать перенос
              <ArrowUpRight size={15} />
            </button>
          </footer>
        </Modal>
      )}
    </div>
  );
}
