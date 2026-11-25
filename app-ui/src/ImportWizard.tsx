import { useEffect, useState } from "react";
import {
  Check,
  ChevronDown,
  FileSpreadsheet,
  Layers,
  LoaderCircle,
} from "lucide-react";
import { Modal, ErrorNote } from "./components";
import {
  basename,
  messageOf,
  roles,
  roleLabels,
  type Backend,
  type FilePlan,
  type Preview,
  type Role,
  type SheetPlan,
} from "./model";

type Config = SheetPlan & { enabled: boolean };
type Item = {
  path: string;
  preview?: Preview;
  plans?: Config[];
  error?: string;
};
type Props = {
  paths: string[];
  api: Backend;
  close: () => void;
  started: (id: number, skipped: string[]) => void;
};

export function ImportWizard({ paths, api, close, started }: Props) {
  const [items, setItems] = useState<Item[]>(paths.map((path) => ({ path })));
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState("");
  const [starting, setStarting] = useState(false);
  useEffect(() => {
    let alive = true;
    async function prepare() {
      for (const path of paths) {
        let update: Partial<Item>;
        try {
          const preview = await api.preview(path);
          update = {
            preview,
            plans: preview.sheets.map((sheet) => ({
              key: sheet.key,
              headers: sheet.headers,
              roles: sheet.headers.map(
                (_, index) => sheet.suggestedRoles[index] ?? null,
              ),
              fixedCurrency: null,
              fixedWeightUnit: null,
              enabled: true,
            })),
          };
        } catch (err) {
          update = { error: messageOf(err) };
        }
        if (!alive) return;
        setItems((prev) =>
          prev.map((item) =>
            item.path === path ? { ...item, ...update } : item,
          ),
        );
      }
    }
    void prepare();
    return () => {
      alive = false;
    };
  }, [paths, api]);
  const item = items[selected];
  const planCount = items.reduce(
    (n, it) => n + (it.plans?.filter((s) => s.enabled).length ?? 0),
    0,
  );
  const loading = items.some((i) => !i.preview && !i.error);
  function change(index: number, edit: Partial<Config>) {
    setError("");
    setItems((prev) =>
      prev.map((it, i) =>
        i !== selected
          ? it
          : {
              ...it,
              plans: it.plans?.map((p, j) =>
                j === index ? { ...p, ...edit } : p,
              ),
            },
      ),
    );
  }
  async function submit() {
    if (starting || loading || !planCount) return;
    const files: FilePlan[] = items.flatMap((i) => {
      const sheets = i.plans
        ?.filter((s) => s.enabled)
        .map(({ key, headers, roles, fixedCurrency, fixedWeightUnit }) => ({
          key,
          headers,
          roles,
          fixedCurrency: fixedCurrency?.trim() || null,
          fixedWeightUnit: fixedWeightUnit?.trim() || null,
        }));
      return sheets?.length ? [{ path: i.path, sheets }] : [];
    });
    for (const file of files) {
      for (const sheet of file.sheets) {
        const assigned = sheet.roles.filter(Boolean);
        if (assigned.length !== new Set(assigned).size) {
          setError(
            "Одна роль назначена нескольким столбцам на листе «" +
              (items
                .find((i) => i.path === file.path)
                ?.preview?.sheets.find((s) => s.key === sheet.key)?.name ??
                sheet.key) +
              "». Выберите разные роли.",
          );
          return;
        }
      }
    }
    setStarting(true);
    try {
      const skipped = items.flatMap((item) => {
        if (item.error) return [basename(item.path) + " — " + item.error];
        if (!item.plans?.some((plan) => plan.enabled))
          return [basename(item.path) + " — нет выбранных листов"];
        return [];
      });
      started(await api.import(files), skipped);
    } catch (err) {
      setError(messageOf(err));
      setStarting(false);
    }
  }
  const onClose = () => {
    if (!starting) close();
  };
  return (
    <Modal title="Предпросмотр импорта" wide close={onClose}>
      <div className="modal-content wizard">
        <div className="file-tabs" aria-label="Выбранные файлы">
          {items.map((it, i) => (
            <button
              aria-pressed={selected === i}
              onClick={() => setSelected(i)}
              key={it.path}
              title={it.path}
            >
              <FileSpreadsheet size={15} />
              {it.preview?.fileName || basename(it.path)}
              {it.error && " ⚠"}
            </button>
          ))}
        </div>
        {loading && items.length > 1 && (
          <p role="status">
            Прочитано файлов{" "}
            {items.filter((it) => it.preview || it.error).length} из{" "}
            {items.length}
          </p>
        )}
        {item.error && <ErrorNote>Файл пропущен. {item.error}</ErrorNote>}
        {!item.preview && !item.error && (
          <p role="status">
            <LoaderCircle className="spin" size={16} /> Читаем файл…
          </p>
        )}
        {item.preview?.sheets.length === 0 && (
          <p>В файле нет доступных листов.</p>
        )}
        {item.preview?.sheets.map((sheet, i) => {
          const plan = item.plans![i];
          return (
            <section className="sheet" key={sheet.key}>
              <header>
                <label>
                  <input
                    type="checkbox"
                    checked={plan.enabled}
                    onChange={(e) => change(i, { enabled: e.target.checked })}
                  />
                  <Layers size={16} />
                  {sheet.name}
                </label>
                <small>{sheet.headers.length} ст.</small>
              </header>
              <fieldset
                disabled={!plan.enabled || starting}
                className="sheet-body"
              >
                <div className="table-scroll">
                  <table className="preview-table">
                    <thead>
                      <tr>
                        <th>Столбец</th>
                        <th>Роль в сводке</th>
                        <th>Пример значения</th>
                      </tr>
                    </thead>
                    <tbody>
                      {sheet.headers.map((h, j) => (
                        <tr key={j}>
                          <th scope="row">{h || "Столбец " + (j + 1)}</th>
                          <td>
                            <select
                              aria-label={"Роль столбца " + h}
                              value={plan.roles[j] || ""}
                              onChange={(e) =>
                                change(i, {
                                  roles: plan.roles.map((r, k) =>
                                    k === j
                                      ? (e.target.value as Role) || null
                                      : r,
                                  ),
                                })
                              }
                            >
                              <option value="">Без роли</option>
                              {roles.map((role) => (
                                <option key={role} value={role}>
                                  {roleLabels[role]}
                                </option>
                              ))}
                            </select>
                          </td>
                          <td>
                            {sheet.rows.find(
                              (row) => row[j] != null && row[j] !== "",
                            )?.[j] ?? "—"}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <details>
                  <summary>
                    <ChevronDown size={15} /> Строки предпросмотра (
                    {sheet.rows.length})
                  </summary>
                  <div className="table-scroll">
                    <table>
                      <thead>
                        <tr>
                          {sheet.headers.map((h, j) => (
                            <th key={j}>{h || "Столбец " + (j + 1)}</th>
                          ))}
                        </tr>
                      </thead>
                      <tbody>
                        {sheet.rows.map((r, k) => (
                          <tr key={k}>
                            {sheet.headers.map((_, j) => (
                              <td key={j}>{r[j] ?? "—"}</td>
                            ))}
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </details>
                <div className="two-fields">
                  <label>
                    Единая валюта
                    <input
                      aria-label={"Единая валюта для " + sheet.name}
                      placeholder="Например, USD"
                      value={plan.fixedCurrency ?? ""}
                      onChange={(e) =>
                        change(i, { fixedCurrency: e.target.value })
                      }
                    />
                  </label>
                  <label>
                    Единица веса для листа
                    <select
                      value={plan.fixedWeightUnit ?? ""}
                      onChange={(e) =>
                        change(i, { fixedWeightUnit: e.target.value || null })
                      }
                    >
                      <option value="">Из данных</option>
                      <option value="kg">кг</option>
                      <option value="g">г</option>
                      <option value="t">т</option>
                      <option value="lb">фунт</option>
                    </select>
                  </label>
                </div>
              </fieldset>
            </section>
          );
        })}
        {items.some((it) => it.error) && (
          <p className="muted">
            Файлы с ошибками предпросмотра пропущены (
            {items.filter((it) => it.error).length}).
          </p>
        )}
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
      <footer className="modal-foot">
        <span>
          Выбрано листов <b>{planCount}</b>
        </span>
        <button onClick={onClose} disabled={starting}>
          Отмена
        </button>
        <button
          className="primary"
          disabled={!planCount || loading || starting}
          onClick={submit}
        >
          <Check size={15} />
          Импортировать
        </button>
      </footer>
    </Modal>
  );
}
