import { useEffect, useState } from "react";
import { ArrowUpRight, Banknote, Layers, Scale } from "lucide-react";
import { Empty, ErrorNote } from "./components";
import {
  count,
  decimal,
  fieldName,
  messageOf,
  operations,
  type Amount,
  type Backend,
  type Dimension,
  type Filter,
  type Query,
  type Summary,
  type Workspace,
} from "./model";

function Amounts({ items }: { items: Amount[] }) {
  return items.length ? (
    <span className="amounts">
      {items.map((item) => (
        <span key={item.currency ?? ""}>
          {decimal(item.amount)} <small>{item.currency ?? "без валюты"}</small>
        </span>
      ))}
    </span>
  ) : (
    <>Нет данных</>
  );
}
const dimensions: Record<Dimension, string> = {
  month: "По месяцам",
  company: "По компаниям",
  product: "По товарам",
};
const names: Record<Dimension, string> = {
  month: "Месяц",
  company: "Компания",
  product: "Товар",
};

export function SummaryView({
  api,
  workspace,
  query,
  drill,
  refresh,
}: {
  api: Backend;
  workspace: Workspace;
  query: Query;
  drill: (f: Filter) => void;
  refresh: () => void;
}) {
  const [dimension, setDimension] = useState<Dimension>("month");
  const [state, setState] = useState<{
    dimension: Dimension;
    query: Query;
    data?: Summary;
    error?: string;
  } | null>(null);
  const [retry, setRetry] = useState(0);
  const stale = workspace.revision !== query.revision;
  useEffect(() => {
    if (stale) return;
    let alive = true;
    setState(null);
    api.summary(query, dimension).then(
      (data) => {
        if (alive) setState({ dimension, query, data });
      },
      (err) => {
        if (alive) setState({ dimension, query, error: messageOf(err) });
      },
    );
    return () => {
      alive = false;
    };
  }, [api, query, dimension, stale, retry]);
  const current =
    state?.dimension === dimension && state.query === query ? state : null;
  const data = current?.data;
  return (
    <>
      {(query.text || query.filters.length > 0) && (
        <div className="query-scope">
          <b>По текущему запросу</b>
          {query.text && <span className="chip">{query.text}</span>}
          {query.filters.map((f, i) => (
            <span key={i} className="chip">
              {fieldName(f.field, workspace.columns)} · {operations[f.op]}{" "}
              {f.value}
            </span>
          ))}
        </div>
      )}
      {stale ? (
        <ErrorNote retry={refresh}>
          Данные изменились. Обновите запрос для пересчёта.
        </ErrorNote>
      ) : current?.error ? (
        <ErrorNote
          retry={() => {
            refresh();
            setRetry((r) => r + 1);
          }}
        >
          {current.error}
        </ErrorNote>
      ) : (
        <>
          <div className="metrics">
            <article>
              <span>
                <Layers size={16} />
                Строки
              </span>
              <strong>{data ? count(data.totalRows) : "…"}</strong>
            </article>
            <article>
              <span>
                <Banknote size={16} />
                Сумма по валютам
              </span>
              <strong>
                {data ? <Amounts items={data.currencyTotals} /> : "…"}
              </strong>
            </article>
            <article>
              <span>
                <Scale size={16} />
                Вес нетто
              </span>
              <strong>
                {data?.netWeightKg != null ? (
                  <>
                    {decimal(data.netWeightKg)} <small>кг</small>
                  </>
                ) : data ? (
                  "Нет данных"
                ) : (
                  "…"
                )}
              </strong>
            </article>
          </div>
          {data && (data.unknownUnitRows > 0 || data.unknownValueRows > 0) && (
            <div className="summary-notes">
              {data.unknownUnitRows > 0 && (
                <p>
                  Неизвестная единица веса · {count(data.unknownUnitRows)} стр.
                  Их вес исключён из итога в кг.
                </p>
              )}
              {data.unknownValueRows > 0 && (
                <p>
                  Не удалось распознать сумму · {count(data.unknownValueRows)}{" "}
                  стр. Эти значения исключены из денежных итогов.
                </p>
              )}
            </div>
          )}
          <section className="results">
            <header className="section-head">
              <h2>Группировка</h2>
              <div className="tabs">
                {(Object.keys(dimensions) as Dimension[]).map((d) => (
                  <button
                    key={d}
                    aria-pressed={dimension === d}
                    onClick={() => setDimension(d)}
                  >
                    {dimensions[d]}
                  </button>
                ))}
              </div>
            </header>
            {!data ? (
              <p className="content-note" role="status">
                Рассчитываем показатели…
              </p>
            ) : !data.groups.length ? (
              <Empty kind="filter" title="Нет данных для группировки" />
            ) : (
              <div className="table-scroll">
                <table className="summary-table">
                  <thead>
                    <tr>
                      <th>{names[dimension]}</th>
                      <th>Строки</th>
                      <th>Доля строк</th>
                      {dimension === "month" && (
                        <th>Изменение числа строк за месяц</th>
                      )}
                      <th>Сумма</th>
                      <th>Вес нетто, кг</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.groups.map((g, i) => (
                      <tr key={i}>
                        <th scope="row">
                          <button
                            className="group-link"
                            onClick={() => drill(g.filter)}
                          >
                            {g.key || "Нет данных"}
                            <ArrowUpRight size={13} />
                          </button>
                        </th>
                        <td>{count(g.rows)}</td>
                        <td>
                          <div className="share">
                            <span className="bar">
                              <i
                                style={{
                                  width:
                                    Math.min(100, Math.max(0, g.sharePercent)) +
                                    "%",
                                }}
                              />
                            </span>
                            {count(g.sharePercent)}%
                          </div>
                        </td>
                        {dimension === "month" && (
                          <td>
                            {g.changePercent == null ? (
                              <span className="muted">Нет данных</span>
                            ) : (
                              (g.changePercent > 0 ? "+" : "") +
                              count(g.changePercent) +
                              "%"
                            )}
                          </td>
                        )}
                        <td>
                          <Amounts items={g.currencyTotals} />
                        </td>
                        <td>{decimal(g.netWeightKg)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            {data && data.groups.length < data.totalGroups && (
              <p className="group-limit">
                Показано групп {count(data.groups.length)} из{" "}
                {count(data.totalGroups)} ·{" "}
                {dimension === "month" ? "последние месяцы" : "по числу строк"}
              </p>
            )}
          </section>
        </>
      )}
    </>
  );
}
