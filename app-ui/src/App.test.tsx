import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { App } from "./App";
import { SearchView } from "./SearchView";
import {
  decimal,
  emptyQuery,
  type Backend,
  type Job,
  type Row,
  type Workspace,
  type Summary,
} from "./model";

beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});
afterEach(() => {
  cleanup();
  localStorage.clear();
});

const empty: Workspace = { revision: 0, rowCount: 0, columns: [], sources: [] };
const imported: Workspace = {
  revision: 1,
  rowCount: 1,
  columns: [
    { id: "c1", label: "Код" },
    { id: "c2", label: "Организация", role: "company" },
  ],
  sources: [{ id: 101, fileName: "поставка.csv", sheet: "Лист 1", rows: 1 }],
};
const row: Row = {
  id: 42,
  sourceName: "поставка.csv",
  sheet: "Лист 1",
  rowNumber: 7,
  values: { c1: "0007", c2: "  Север  " },
};
const summary: Summary = {
  totalGroups: 1,
  totalRows: 1,
  currencyTotals: [
    { currency: "USD", amount: "9007199254740993.01" },
    { currency: null, amount: "0.1" },
  ],
  netWeightKg: null,
  unknownUnitRows: 1,
  unknownValueRows: 1,
  groups: [
    {
      key: "Север",
      rows: 1,
      sharePercent: 100,
      changePercent: null,
      currencyTotals: [],
      netWeightKg: null,
      filter: { field: "@company", op: "eq", value: "Север" },
    },
  ],
};
const complete: Job = {
  id: 3,
  state: "complete",
  kind: "import",
  processed: 1,
  total: 1,
  message: "",
  result: {
    rows: 1,
    skippedSheets: 0,
    path: null,
    files: [
      {
        fileName: "поставка.csv",
        insertedRows: 1,
        skippedSheets: 0,
        error: null,
      },
    ],
  },
};
function mockApi() {
  return {
    workspace: vi.fn<Backend["workspace"]>().mockResolvedValue(imported),
    chooseFiles: vi
      .fn<Backend["chooseFiles"]>()
      .mockResolvedValue(["/data/поставка.csv"]),
    chooseLegacy: vi
      .fn<Backend["chooseLegacy"]>()
      .mockResolvedValue("/data/base_search.db"),
    chooseExport: vi
      .fn<Backend["chooseExport"]>()
      .mockResolvedValue("/out/view.csv"),
    preview: vi.fn<Backend["preview"]>().mockResolvedValue({
      path: "/data/поставка.csv",
      fileName: "поставка.csv",
      sheets: [
        {
          key: "one",
          name: "Лист 1",
          headers: ["Код", "Организация"],
          rows: [["0007", "  Север  "]],
          suggestedRoles: [null, "company"],
        },
        {
          key: "two",
          name: "Примечания",
          headers: ["Текст"],
          rows: [["Черновик"]],
          suggestedRoles: [null],
        },
      ],
    }),
    import: vi.fn<Backend["import"]>().mockResolvedValue(3),
    migrate: vi.fn<Backend["migrate"]>().mockResolvedValue(4),
    search: vi
      .fn<Backend["search"]>()
      .mockResolvedValue({ revision: 1, totalRows: 1, rows: [row] }),
    detail: vi.fn<Backend["detail"]>().mockResolvedValue(row),
    summary: vi.fn<Backend["summary"]>().mockResolvedValue(summary),
    export: vi.fn<Backend["export"]>().mockResolvedValue(5),
    job: vi.fn<Backend["job"]>().mockImplementation(async (id) =>
      id === 5
        ? {
            ...complete,
            kind: "export",
            id,
            result: {
              rows: 1,
              skippedSheets: 0,
              files: [],
              path: "/out/view.csv",
            },
          }
        : complete,
    ),
    cancel: vi.fn<Backend["cancel"]>().mockResolvedValue(undefined),
  } satisfies Backend;
}
const btn = (name: string) => screen.getByRole("button", { name });
const press = (name: string) => fireEvent.click(btn(name));

describe("Рабочая область", () => {
  it("previews every selected sheet, imports the configured subset, opens raw detail and exports only the last applied query in visible column order", async () => {
    const api = mockApi();
    api.workspace.mockResolvedValueOnce(empty);
    render(<App api={api} />);
    await screen.findByText("Добавьте первую", { exact: false });
    press("Добавить файлы");
    await screen.findByRole("checkbox", { name: "Примечания" });
    fireEvent.click(screen.getByRole("checkbox", { name: "Примечания" }));
    fireEvent.change(screen.getByLabelText("Роль столбца Код"), {
      target: { value: "product" },
    });
    fireEvent.change(screen.getByLabelText("Единая валюта для Лист 1"), {
      target: { value: "EUR" },
    });
    press("Импортировать");
    await screen.findByText("Импорт · завершён");
    expect(api.import).toHaveBeenCalledWith([
      {
        path: "/data/поставка.csv",
        sheets: [
          {
            key: "one",
            headers: ["Код", "Организация"],
            roles: ["product", "company"],
            fixedCurrency: "EUR",
            fixedWeightUnit: null,
          },
        ],
      },
    ]);
    await waitFor(() => expect(api.workspace).toHaveBeenCalledTimes(2));
    await screen.findByRole("heading", { name: "Поиск" });
    await screen.findByText("0007");
    fireEvent.change(screen.getByLabelText("Поисковый запрос"), {
      target: { value: "Север" },
    });
    press("Найти");
    await screen.findByText("Текст · Север");
    press("Код");
    await waitFor(() =>
      expect(api.search).toHaveBeenLastCalledWith(
        {
          ...emptyQuery,
          text: "Север",
          revision: 1,
          sort: { field: "c1", direction: "asc" },
        },
        0,
        50,
      ),
    );
    await screen.findByText("0007");
    press("Открыть строку 1");
    const drawer = await screen.findByRole("dialog", {
      name: "Карточка записи",
    });
    await within(drawer).findByText("ID 42");
    expect(within(drawer).getByText("Север").textContent).toBe("  Север  ");
    expect(api.detail).toHaveBeenCalledWith(42, 1);
    press("Закрыть");
    press("Столбцы");
    press("Поднять Организация");
    press("Готово");
    fireEvent.change(screen.getByLabelText("Поисковый запрос"), {
      target: { value: "другой черновик" },
    });
    press("Выгрузить");
    fireEvent.change(screen.getByLabelText("Формат файла"), {
      target: { value: "csv" },
    });
    press("Сохранить файл");
    await screen.findByText("/out/view.csv");
    expect(api.export).toHaveBeenCalledWith(
      {
        ...emptyQuery,
        text: "Север",
        revision: 1,
        sort: { field: "c1", direction: "asc" },
      },
      "/out/view.csv",
      ["c2", "c1"],
    );
  });

  it("submits the current value in the search field even before a React change is committed", async () => {
    const api = mockApi();
    render(<App api={api} />);
    await screen.findByText("0007");
    const field = screen.getByRole<HTMLInputElement>("textbox", {
      name: "Поисковый запрос",
    });
    const setValue = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )?.set;
    setValue?.call(field, "coffee");
    fireEvent.submit(field.form!);
    await waitFor(() =>
      expect(api.search).toHaveBeenLastCalledWith(
        expect.objectContaining({ text: "coffee" }),
        0,
        50,
      ),
    );
  });

  it("preserves the source header spellings, duplicate labels and original order in the detail drawer", async () => {
    const api = mockApi();
    api.detail.mockResolvedValue({
      ...row,
      sourceColumns: [
        { id: "c2", label: "ОРГАНИЗАЦИЯ" },
        { id: "c1", label: "КОД" },
        { id: "c3", label: "КОД" },
      ],
      values: { c1: "0007", c2: "  Север  ", c3: "" },
    });
    render(<App api={api} />);
    await screen.findByText("0007");
    press("Открыть строку 1");
    const drawer = await screen.findByRole("dialog", {
      name: "Карточка записи",
    });
    await within(drawer).findByText("ID 42");
    const fields = Array.from(
      drawer.querySelectorAll(".raw > div"),
      (field) => [
        field.querySelector("dt")?.textContent,
        field.querySelector("dd")?.textContent,
      ],
    );
    expect(fields).toEqual([
      ["ОРГАНИЗАЦИЯ", "  Север  "],
      ["КОД", "0007"],
      ["КОД", "Пустая строка"],
    ]);
  });

  it("drills down from a summary group while retaining the existing applied query", async () => {
    const api = mockApi();
    render(<App api={api} />);
    await screen.findByText("0007");
    fireEvent.change(screen.getByLabelText("Поисковый запрос"), {
      target: { value: "партия" },
    });
    press("Найти");
    press("Сводка");
    press("По компаниям");
    await screen.findByText("Не удалось распознать сумму", { exact: false });
    expect(
      screen.getByText("9 007 199 254 740 993,01", {
        normalizer: (s) => s.trim(),
      }),
    ).toBeTruthy();
    press("Север");
    await waitFor(() =>
      expect(api.search).toHaveBeenLastCalledWith(
        {
          ...emptyQuery,
          text: "партия",
          revision: 1,
          filters: [summary.groups[0].filter],
        },
        0,
        50,
      ),
    );
  });

  it("keeps a working file importable if another selected preview fails", async () => {
    const api = mockApi();
    api.workspace.mockResolvedValue(empty);
    api.chooseFiles.mockResolvedValue([
      "/data/нечитаемый.csv",
      "/data/поставка.csv",
    ]);
    api.preview.mockRejectedValueOnce(new Error("Ошибка чтения"));
    render(<App api={api} />);
    await screen.findByText("Добавьте первую", { exact: false });
    press("Добавить файлы");
    await screen.findByText("Файл пропущен. Ошибка чтения");
    await waitFor(() =>
      expect((btn("Импортировать") as HTMLButtonElement).disabled).toBe(false),
    );
    press("поставка.csv");
    await screen.findByRole("checkbox", { name: "Примечания" });
    press("Импортировать");
    await screen.findByText("Импорт · завершён");
    expect(screen.getByText("нечитаемый.csv — Ошибка чтения")).toBeTruthy();
    expect(api.import.mock.calls[0][0].map((f) => f.path)).toEqual([
      "/data/поставка.csv",
    ]);
  });

  it("cancels migration, refreshes committed metadata and surfaces a cancelled outcome", async () => {
    const api = mockApi();
    api.workspace.mockResolvedValue(empty);
    api.job.mockResolvedValue({
      ...complete,
      id: 4,
      kind: "migration",
      state: "running",
      result: null,
    });
    api.cancel.mockImplementation(async () => {
      api.job.mockResolvedValue({
        ...complete,
        id: 4,
        kind: "migration",
        state: "cancelled",
        result: null,
      });
    });
    render(<App api={api} />);
    await screen.findByText("Добавьте первую", { exact: false });
    press("Перенести базу 2.2.0");
    await screen.findByRole("dialog");
    press("Начать перенос");
    await screen.findByText("Перенос базы · выполняется");
    press("Отменить операцию");
    await screen.findByText("Перенос базы · отменён");
    expect(api.migrate).toHaveBeenCalledWith("/data/base_search.db");
    expect(api.workspace).toHaveBeenCalledTimes(2);
  });
  it("drops stale replies and disables export on a mismatched database revision", async () => {
    const api = mockApi();
    const query = { ...emptyQuery, revision: 1 };
    let resolveOld!: (result: {
      revision: number;
      totalRows: number;
      rows: Row[];
    }) => void;
    api.search.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveOld = resolve;
      }),
    );
    const props = {
      api,
      workspace: imported,
      query,
      apply: vi.fn(),
      jobStarted: vi.fn(),
      busy: false,
      reload: vi.fn(),
    };
    const view = render(<SearchView {...props} />);
    const newer = { ...query, text: "новый" };
    view.rerender(<SearchView {...props} query={newer} />);
    await screen.findByText("0007");
    await act(async () => resolveOld({ revision: 1, totalRows: 0, rows: [] }));
    expect(screen.queryByText("Совпадений нет")).toBeNull();
    view.rerender(
      <SearchView
        {...props}
        query={newer}
        workspace={{ ...imported, revision: 2 }}
      />,
    );
    expect((btn("Выгрузить") as HTMLButtonElement).disabled).toBe(true);
    press("Повторить");
    expect(props.apply).toHaveBeenCalledWith({ ...newer, revision: 2 });
  });
  it("dismissing the native file chooser leaves the database untouched", async () => {
    const api = mockApi();
    api.workspace.mockResolvedValue(empty);
    api.chooseFiles.mockResolvedValue([]);
    render(<App api={api} />);
    await screen.findByText("Добавьте первую", { exact: false });
    press("Добавить файлы");
    await waitFor(() =>
      expect((btn("Добавить файлы") as HTMLButtonElement).disabled).toBe(false),
    );
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.preview).not.toHaveBeenCalled();
    expect(api.import).not.toHaveBeenCalled();
  });
  it("applies a raw-column filter to all pages and clears it from the visible chip", async () => {
    const api = mockApi();
    api.search.mockImplementation(async (query, offset) => ({
      revision: 1,
      totalRows: 53,
      rows: Array.from({ length: offset ? 3 : 50 }, (_, index) => ({
        ...row,
        id: offset + index + 1,
        values: { ...row.values, c1: String(offset + index + 1) },
      })),
    }));
    render(<App api={api} />);
    await screen.findByRole("button", { name: "Открыть строку 1" });
    press("Фильтры");
    fireEvent.change(screen.getByLabelText("Поле фильтра 1"), {
      target: { value: "c1" },
    });
    fireEvent.change(screen.getByLabelText("Условие"), {
      target: { value: "prefix" },
    });
    fireEvent.change(screen.getByLabelText("Значение фильтра"), {
      target: { value: "0" },
    });
    press("Применить");
    const applied = {
      ...emptyQuery,
      revision: 1,
      filters: [{ field: "c1", op: "prefix", value: "0" }],
    };
    await waitFor(() =>
      expect(api.search).toHaveBeenLastCalledWith(applied, 0, 50),
    );
    await screen.findByRole("button", { name: "Следующая страница" });
    press("Следующая страница");
    await screen.findByRole("button", { name: "Открыть строку 51" });
    expect(api.search).toHaveBeenLastCalledWith(applied, 50, 50);
    press("Код · Начинается с 0");
    await waitFor(() =>
      expect(api.search).toHaveBeenLastCalledWith(
        { ...emptyQuery, revision: 1 },
        0,
        50,
      ),
    );
  });

  it("reports a native export failure and retains the applied query for retry", async () => {
    const api = mockApi();
    api.export.mockRejectedValue(new Error("Недостаточно места"));
    api.chooseExport.mockResolvedValue("/out/table.xlsx");
    render(<App api={api} />);
    await screen.findByText("0007");
    press("Выгрузить");
    press("Сохранить файл");
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Недостаточно места",
    );
    expect(
      screen.getByRole("dialog", { name: "Экспорт результатов" }),
    ).toBeTruthy();
    expect(api.job).not.toHaveBeenCalled();
  });

  it("formats money without losing decimal precision", () => {
    expect(decimal("9007199254740993.03")).toBe("9 007 199 254 740 993,03");
    expect(decimal(null)).toBe("Нет данных");
  });
});
