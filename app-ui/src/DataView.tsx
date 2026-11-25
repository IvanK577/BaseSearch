import {
  ArrowRight,
  DatabaseBackup,
  FilePlus2,
  FileSpreadsheet,
  Files,
  Layers,
} from "lucide-react";
import { count, type Workspace } from "./model";

export function Welcome({
  add,
  migrate,
  busy,
}: {
  add: () => void;
  migrate: () => void;
  busy: boolean;
}) {
  return (
    <section className="welcome">
      <div className="welcome-content">
        <div className="welcome-icon">
          <Layers size={23} strokeWidth={1.5} />
        </div>
        <h2>
          Добавьте первую
          <br />
          таблицу
        </h2>
        <p>
          Объедините файлы в локальной базе, чтобы искать записи и сравнивать
          показатели.
        </p>
        <div className="actions">
          <button className="primary" disabled={busy} onClick={add}>
            <FilePlus2 size={16} />
            Добавить файлы
          </button>
          <button disabled={busy} onClick={migrate}>
            <DatabaseBackup size={16} />
            Перенести базу 2.2.0
          </button>
        </div>
      </div>
      <div className="welcome-art" aria-hidden="true">
        <div className="art-stack">
          <div className="art-page rear" />
          <div className="art-page middle" />
          <div className="art-page front">
            <div className="art-heading">
              <span />
              <span />
              <span />
            </div>
            <div className="art-table">
              {Array.from({ length: 24 }, (_, i) => (
                <i key={i} />
              ))}
            </div>
          </div>
          <div className="art-lens">
            <span />
          </div>
        </div>
      </div>
    </section>
  );
}
export function DataView({ workspace }: { workspace: Workspace }) {
  const files = new Set(workspace.sources.map((s) => s.fileName)).size;
  return (
    <section className="results">
      <header className="section-head">
        <div>
          <h2>Источники</h2>
          <span className="number-tag">{count(workspace.sources.length)}</span>
        </div>
        {files > 0 && <small>Файлов · {count(files)}</small>}
      </header>
      {workspace.sources.length ? (
        <div className="table-scroll">
          <table className="source-table">
            <thead>
              <tr>
                <th>Имя файла</th>
                <th>Лист</th>
                <th>Строки</th>
              </tr>
            </thead>
            <tbody>
              {workspace.sources.map((s) => (
                <tr key={s.id}>
                  <th scope="row">
                    <FileSpreadsheet size={17} />
                    {s.fileName}
                  </th>
                  <td>{s.sheet}</td>
                  <td>{count(s.rows)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="source-placeholder">
          <div>
            <Files size={22} strokeWidth={1.5} />
          </div>
          <p>
            <strong>Источников пока нет</strong>
            <span>Здесь появятся имена файлов, листы и количество строк.</span>
          </p>
        </div>
      )}
    </section>
  );
}
export function OpenData({ open }: { open: () => void }) {
  return (
    <button onClick={open}>
      К источникам
      <ArrowRight size={15} />
    </button>
  );
}
