import { useEffect, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowDownUp,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Columns3,
  Download,
  Filter as FilterIcon,
  Plus,
  Search as SearchIcon,
  X,
} from "lucide-react";
import { Empty, ErrorNote, Modal } from "./components";
import {
  count,
  emptyQuery,
  fieldName,
  messageOf,
  operations,
  PAGE_SIZE,
  roles,
  roleLabels,
  type Backend,
  type Filter,
  type Op,
  type Query,
  type Results,
  type Row,
  type Workspace,
} from "./model";

const columnPreference = "base-search.visible-columns.v1";

function savedColumns(): string[] | null {
  try {
    const ids: unknown = JSON.parse(
      localStorage.getItem(columnPreference) ?? "null",
    );
    return Array.isArray(ids) && ids.every((id) => typeof id === "string")
      ? [...new Set<string>(ids)]
      : null;
  } catch {
    return null;
  }
}

type Props = {
  api: Backend;
  workspace: Workspace;
  query: Query;
  apply: (q: Query) => void;
  jobStarted: (id: number) => void;
  busy: boolean;
  reload: () => void;
};
export function SearchView({
  api,
  workspace,
  query,
  apply,
  jobStarted,
  busy,
  reload,
}: Props) {
  const [draft, setDraft] = useState(query.text);
  const [filters, setFilters] = useState<Filter[]>(query.filters);
  const [editing, setEditing] = useState(false);
  const [page, setPage] = useState(0);
  const [choice, setChoice] = useState<string[] | null>(savedColumns);
  useEffect(() => {
    if (choice) {
      try {
        localStorage.setItem(columnPreference, JSON.stringify(choice));
      } catch {
        /* Keep the selection available for this session. */
      }
    }
  }, [choice]);
  const [modal, setModal] = useState<"columns" | "export" | null>(null);
  const [format, setFormat] = useState<"csv" | "xlsx">("xlsx");
  const [all, setAll] = useState(false);
  const [exportBusy, setExportBusy] = useState(false);
  const [exportError, setExportError] = useState("");
  const [retry, setRetry] = useState(0);
  const [fetchState, setFetch] = useState<{
    query: Query;
    page: number;
    data?: Results;
    error?: string;
  } | null>(null);
  const [detail, setDetail] = useState<{ id: number; revision: number } | null>(
    null,
  );
  const ref = useRef<HTMLInputElement>(null);
  const stale = query.revision !== workspace.revision;
  const columns = workspace.columns;
  const byId = new Map(columns.map((column) => [column.id, column]));
  const saved = choice?.filter((id) => byId.has(id));
  const selected = saved?.length
    ? saved
    : columns.slice(0, 8).map((column) => column.id);
  const fields = selected.map((id) => byId.get(id)!);
  useEffect(() => {
    setDraft(query.text);
    setFilters(query.filters);
    setPage(0);
    setDetail(null);
  }, [query]);
  useEffect(() => {
    if (stale) return;
    let current = true;
    setFetch(null);
    api.search(query, page * PAGE_SIZE, PAGE_SIZE).then(
      (data) => {
        if (current)
          setFetch(
            data.revision === query.revision
              ? { query, page, data }
              : {
                  query,
                  page,
                  error: "Версия базы изменилась. Обновите результаты.",
                },
          );
      },
      (err) => {
        if (current) setFetch({ query, page, error: messageOf(err) });
      },
    );
    return () => {
      current = false;
    };
  }, [api, query, page, retry, stale]);
  useEffect(() => {
    function shortcut(e: KeyboardEvent) {
      if (
        (e.ctrlKey || e.metaKey) &&
        e.key.toLowerCase() === "k" &&
        !document.querySelector("dialog[open]")
      ) {
        e.preventDefault();
        ref.current?.focus();
      }
    }
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, []);
  const state =
    !stale && fetchState?.query === query && fetchState.page === page
      ? fetchState
      : null;
  const data = state?.data;
  function submit() {
    if (filters.some((f) => f.op !== "empty" && !f.value.trim())) {
      setEditing(true);
      return;
    }
    apply({
      ...query,
      text: draft.trim(),
      filters: filters.map((f) => ({
        ...f,
        value: f.op === "empty" ? "" : f.value,
      })),
      revision: workspace.revision,
    });
    setEditing(false);
  }
  function changeFilter(i: number, patch: Partial<Filter>) {
    setFilters((old) => old.map((f, k) => (i === k ? { ...f, ...patch } : f)));
  }
  function move(id: string, delta: number) {
    const next = [...selected],
      i = next.indexOf(id);
    [next[i], next[i + delta]] = [next[i + delta], next[i]];
    setChoice(next);
  }
  async function doExport() {
    if (!data || stale || busy || exportBusy) return;
    setExportBusy(true);
    setExportError("");
    try {
      const path = await api.chooseExport(format);
      if (!path) return;
      if (!path.toLowerCase().endsWith("." + format)) {
        throw Error("Укажите расширение ." + format + " в окне сохранения.");
      }
      jobStarted(await api.export(query, path, all ? null : selected));
      setModal(null);
    } catch (err) {
      setExportError(messageOf(err));
    } finally {
      setExportBusy(false);
    }
  }
  return (
    <>
      <section className="search-panel">
        <form
          className="quick-search"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <SearchIcon size={21} />
          <input
            ref={ref}
            aria-label="Поисковый запрос"
            placeholder="Поиск по всем столбцам"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
          <kbd>{/Mac/i.test(navigator.platform) ? "⌘ K" : "Ctrl K"}</kbd>
          <button className="primary" type="submit">
            Найти
            <ArrowRight size={15} />
          </button>
        </form>
        <div className="filter-bar">
          <button
            className="flat"
            aria-expanded={editing}
            onClick={() => {
              setEditing(!editing);
              if (editing) setFilters(query.filters);
              else if (!filters.length)
                setFilters([
                  {
                    field: columns[0]?.id ?? "@company",
                    op: "contains",
                    value: "",
                  },
                ]);
            }}
          >
            <FilterIcon size={15} />
            Фильтры
          </button>
          {query.text && <span className="chip">Текст · {query.text}</span>}
          {query.filters.map((f, i) => (
            <button
              className="chip"
              key={i}
              title="Снять фильтр"
              onClick={() =>
                apply({
                  ...query,
                  filters: query.filters.filter((_, j) => i !== j),
                  revision: workspace.revision,
                })
              }
            >
              {fieldName(f.field, columns)} · {operations[f.op]} {f.value}
              <X size={12} />
            </button>
          ))}
          {(query.text || query.filters.length > 0) && (
            <button
              className="text-btn"
              onClick={() => {
                apply({ ...emptyQuery, revision: workspace.revision });
                setEditing(false);
              }}
            >
              Сбросить
            </button>
          )}
        </div>
        {editing && (
          <form
            className="filter-editor"
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            {filters.map((f, i) => (
              <div className="filter-row" key={i}>
                <select
                  aria-label={"Поле фильтра " + (i + 1)}
                  value={f.field}
                  onChange={(e) => changeFilter(i, { field: e.target.value })}
                >
                  <optgroup label="Столбцы">
                    {columns.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.label}
                      </option>
                    ))}
                  </optgroup>
                  <optgroup label="Роли">
                    {roles.map((r) => (
                      <option key={r} value={"@" + r}>
                        {roleLabels[r]}
                      </option>
                    ))}
                  </optgroup>
                </select>
                <select
                  aria-label="Условие"
                  value={f.op}
                  onChange={(e) =>
                    changeFilter(i, { op: e.target.value as Op })
                  }
                >
                  {Object.entries(operations).map(([k, v]) => (
                    <option key={k} value={k}>
                      {v}
                    </option>
                  ))}
                </select>
                {f.op !== "empty" && (
                  <input
                    aria-label="Значение фильтра"
                    placeholder="Значение"
                    required
                    value={f.value}
                    onChange={(e) => changeFilter(i, { value: e.target.value })}
                  />
                )}
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Удалить условие"
                  onClick={() => setFilters(filters.filter((_, j) => i !== j))}
                >
                  <X size={15} />
                </button>
              </div>
            ))}
            <div className="actions">
              <button
                type="button"
                onClick={() =>
                  setFilters([
                    ...filters,
                    {
                      field: columns[0]?.id ?? "@company",
                      op: "contains",
                      value: "",
                    },
                  ])
                }
              >
                <Plus size={15} />
                Условие
              </button>
              <button className="dark" type="submit">
                Применить
              </button>
              <button
                type="button"
                className="text-btn"
                onClick={() => {
                  setFilters(query.filters);
                  setEditing(false);
                }}
              >
                Отмена
              </button>
            </div>
          </form>
        )}
      </section>
      <section className="results">
        <header className="section-head">
          <div>
            <h2>Результаты</h2>
            {data && (
              <span className="number-tag">{count(data.totalRows)}</span>
            )}
          </div>
          <div className="actions">
            <button onClick={() => setModal("columns")}>
              <Columns3 size={15} />
              Столбцы
            </button>
            <button
              disabled={busy || !data?.totalRows}
              onClick={() => {
                setExportError("");
                setModal("export");
              }}
            >
              <Download size={15} />
              Экспорт
            </button>
          </div>
        </header>
        {stale ? (
          <ErrorNote
            retry={() => {
              apply({ ...query, revision: workspace.revision });
            }}
          >
            Данные изменились. Обновите запрос, чтобы открыть актуальные
            результаты.
          </ErrorNote>
        ) : state?.error ? (
          <ErrorNote
            retry={() => {
              reload();
              setRetry((n) => n + 1);
            }}
          >
            {state.error}
          </ErrorNote>
        ) : !data ? (
          <p role="status" className="content-note">
            Выполняем поиск…
          </p>
        ) : data.totalRows === 0 ? (
          <Empty kind="search" title="Совпадений нет">
            <p>Измените поисковый запрос или условия фильтрации.</p>
          </Empty>
        ) : (
          <>
            <div className="table-scroll result-scroll">
              <table className="records">
                <thead>
                  <tr>
                    <th className="index-col" scope="col">
                      №
                    </th>
                    {fields.map((c) => (
                      <th
                        key={c.id}
                        scope="col"
                        aria-sort={
                          query.sort?.field === c.id
                            ? query.sort.direction === "asc"
                              ? "ascending"
                              : "descending"
                            : "none"
                        }
                      >
                        <button
                          onClick={() =>
                            apply({
                              ...query,
                              sort: {
                                field: c.id,
                                direction:
                                  query.sort?.field === c.id &&
                                  query.sort.direction === "asc"
                                    ? "desc"
                                    : "asc",
                              },
                            })
                          }
                        >
                          {c.label}
                          {query.sort?.field === c.id ? (
                            query.sort.direction === "asc" ? (
                              <ArrowUp size={12} />
                            ) : (
                              <ArrowDown size={12} />
                            )
                          ) : (
                            <ArrowDownUp size={12} />
                          )}
                        </button>
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {data.rows.map((row, i) => (
                    <tr
                      key={row.id}
                      onClick={() =>
                        setDetail({ id: row.id, revision: data.revision })
                      }
                    >
                      <td className="index-col">
                        <button
                          className="row-open"
                          aria-label={
                            "Открыть строку " + (page * PAGE_SIZE + i + 1)
                          }
                          onClick={(e) => {
                            e.stopPropagation();
                            setDetail({ id: row.id, revision: data.revision });
                          }}
                        >
                          {count(page * PAGE_SIZE + i + 1)}
                        </button>
                      </td>
                      {fields.map((c) => (
                        <td key={c.id} title={row.values[c.id] ?? ""}>
                          {row.values[c.id] ?? (
                            <span className="missing">—</span>
                          )}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <footer className="pagination">
              <span>
                {count(page * PAGE_SIZE + 1)}–
                {count(Math.min((page + 1) * PAGE_SIZE, data.totalRows))} из{" "}
                {count(data.totalRows)}
              </span>
              <div>
                <button
                  className="icon-button"
                  aria-label="Предыдущая страница"
                  disabled={!page}
                  onClick={() => setPage(page - 1)}
                >
                  <ArrowLeft size={16} />
                </button>
                <span>
                  {page + 1} / {Math.ceil(data.totalRows / PAGE_SIZE)}
                </span>
                <button
                  className="icon-button"
                  aria-label="Следующая страница"
                  disabled={(page + 1) * PAGE_SIZE >= data.totalRows}
                  onClick={() => setPage(page + 1)}
                >
                  <ArrowRight size={16} />
                </button>
              </div>
            </footer>
          </>
        )}
      </section>
      {detail && !stale && (
        <RecordDetail
          api={api}
          workspace={workspace}
          {...detail}
          close={() => setDetail(null)}
        />
      )}
      {modal === "columns" && (
        <Modal title="Столбцы таблицы" close={() => setModal(null)}>
          <div className="modal-content">
            <p className="muted">Выберите видимые столбцы и их порядок.</p>
            <div className="column-list">
              {[
                ...selected,
                ...columns
                  .map((c) => c.id)
                  .filter((id) => !selected.includes(id)),
              ].map((id) => (
                <div key={id}>
                  <label>
                    <input
                      type="checkbox"
                      checked={selected.includes(id)}
                      disabled={selected.includes(id) && selected.length === 1}
                      onChange={(e) =>
                        setChoice(
                          e.target.checked
                            ? [...selected, id]
                            : selected.filter((k) => k !== id),
                        )
                      }
                    />
                    {fieldName(id, columns)}
                  </label>
                  {selected.includes(id) && (
                    <div>
                      <button
                        aria-label={"Поднять " + fieldName(id, columns)}
                        className="icon-button"
                        disabled={selected[0] === id}
                        onClick={() => move(id, -1)}
                      >
                        <ArrowUp size={15} />
                      </button>
                      <button
                        aria-label={"Опустить " + fieldName(id, columns)}
                        className="icon-button"
                        disabled={selected.at(-1) === id}
                        onClick={() => move(id, 1)}
                      >
                        <ArrowDown size={15} />
                      </button>
                    </div>
                  )}
                </div>
              ))}
            </div>
          </div>
          <footer className="modal-foot">
            <button className="primary" onClick={() => setModal(null)}>
              Готово
            </button>
          </footer>
        </Modal>
      )}
      {modal === "export" && (
        <Modal
          title="Экспорт результатов"
          close={() => {
            if (!exportBusy) setModal(null);
          }}
        >
          <div className="modal-content form-stack">
            <p>
              Строк по применённому запросу <b>{count(data?.totalRows ?? 0)}</b>
              . Порядок сортировки сохранится.
            </p>
            <label>
              Формат файла
              <select
                value={format}
                onChange={(e) => setFormat(e.target.value as "csv" | "xlsx")}
              >
                <option value="xlsx">Excel (.xlsx)</option>
                <option value="csv">CSV (.csv)</option>
              </select>
            </label>
            <fieldset className="radio-group">
              <legend>Столбцы</legend>
              <label>
                <input
                  type="radio"
                  checked={!all}
                  onChange={() => setAll(false)}
                />
                Видимые, в порядке таблицы ({selected.length})
              </label>
              <label>
                <input
                  type="radio"
                  checked={all}
                  onChange={() => setAll(true)}
                />
                Все столбцы ({columns.length})
              </label>
            </fieldset>
            {exportError && <ErrorNote>{exportError}</ErrorNote>}
          </div>
          <footer className="modal-foot">
            <button disabled={exportBusy} onClick={() => setModal(null)}>
              Отмена
            </button>
            <button
              className="primary"
              disabled={exportBusy || !data?.totalRows || busy}
              onClick={doExport}
            >
              <Download size={15} />
              Сохранить файл
            </button>
          </footer>
        </Modal>
      )}
    </>
  );
}

function RecordDetail({
  api,
  workspace,
  id,
  revision,
  close,
}: {
  api: Backend;
  workspace: Workspace;
  id: number;
  revision: number;
  close: () => void;
}) {
  const [row, setRow] = useState<Row | null>();
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    api.detail(id, revision).then(
      (value) => {
        if (active) setRow(value);
      },
      (err) => {
        if (active) setError(messageOf(err));
      },
    );
    return () => {
      active = false;
    };
  }, [api, id, revision]);
  const detailColumns =
    row?.sourceColumns ??
    workspace.columns.filter(
      (column) => row && Object.hasOwn(row.values, column.id),
    );
  return (
    <Modal title="Карточка записи" drawer close={close}>
      <div className="modal-content">
        {error ? (
          <ErrorNote>{error}</ErrorNote>
        ) : row === undefined ? (
          <p role="status">Загружаем запись…</p>
        ) : row === null ? (
          <ErrorNote>Запись больше недоступна. Обновите запрос.</ErrorNote>
        ) : (
          <>
            <div className="record-id">ID {row.id}</div>
            <h3>Источник</h3>
            <dl className="detail-fields">
              <dt>Файл</dt>
              <dd>{row.sourceName}</dd>
              <dt>Лист</dt>
              <dd>{row.sheet}</dd>
              <dt>Строка в источнике</dt>
              <dd>{row.rowNumber ?? "Нет данных"}</dd>
            </dl>
            <h3>Исходные значения</h3>
            <dl className="detail-fields raw">
              {detailColumns.map(({ id, label }, i) => {
                const value = row.values[id] ?? null;
                return (
                  <div key={i}>
                    <dt>{label}</dt>
                    <dd>
                      {value === null ? (
                        <i>Нет значения</i>
                      ) : value === "" ? (
                        <i>Пустая строка</i>
                      ) : (
                        value
                      )}
                    </dd>
                  </div>
                );
              })}
            </dl>
          </>
        )}
      </div>
    </Modal>
  );
}
