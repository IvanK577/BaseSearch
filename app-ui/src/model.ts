export const roles = [
  "date",
  "company",
  "product",
  "value",
  "currency",
  "weight",
  "weightUnit",
] as const;
export type Role = (typeof roles)[number];
export type Dimension = "month" | "company" | "product";
export const roleLabels: Record<Role, string> = {
  date: "Дата",
  company: "Компания",
  product: "Товар",
  value: "Сумма",
  currency: "Валюта",
  weight: "Вес нетто",
  weightUnit: "Единица веса",
};
export interface Column {
  id: string;
  label: string;
  role?: Role | null;
}
export interface Source {
  id: number | string;
  fileName: string;
  sheet: string;
  rows: number;
}
export interface Workspace {
  revision: number;
  rowCount: number;
  columns: Column[];
  sources: Source[];
}
export interface Row {
  id: number;
  sourceName: string;
  sheet: string;
  rowNumber: number | null;
  sourceColumns?: Column[];
  values: Record<string, string | null>;
}
export interface Sheet {
  key: string;
  name: string;
  headers: string[];
  rows: (string | null)[][];
  suggestedRoles: (Role | null)[];
}
export interface Preview {
  path: string;
  fileName: string;
  sheets: Sheet[];
}
export interface SheetPlan {
  key: string;
  headers: string[];
  roles: (Role | null)[];
  fixedCurrency: string | null;
  fixedWeightUnit: string | null;
}
export interface FilePlan {
  path: string;
  sheets: SheetPlan[];
}
export type Op = "eq" | "contains" | "prefix" | "from" | "to" | "empty";
export interface Filter {
  field: string;
  op: Op;
  value: string;
}
export interface Query {
  text: string;
  filters: Filter[];
  sort: { field: string; direction: "asc" | "desc" } | null;
  revision: number | null;
}
export const emptyQuery: Query = {
  text: "",
  filters: [],
  sort: null,
  revision: null,
};
export interface Results {
  revision: number;
  totalRows: number;
  rows: Row[];
}
export interface Amount {
  currency: string | null;
  amount: string;
}
export interface Group {
  key: string;
  rows: number;
  sharePercent: number;
  changePercent: number | null;
  currencyTotals: Amount[];
  netWeightKg: string | null;
  filter: Filter;
}
export interface Summary {
  totalGroups: number;
  totalRows: number;
  currencyTotals: Amount[];
  netWeightKg: string | null;
  unknownUnitRows: number;
  unknownValueRows: number;
  groups: Group[];
}
export interface Job {
  id: number;
  kind: "import" | "migration" | "export";
  state: "running" | "complete" | "failed" | "cancelled";
  processed: number;
  total: number | null;
  message: string;
  result: {
    rows: number;
    skippedSheets: number;
    files: {
      fileName: string;
      insertedRows: number;
      skippedSheets: number;
      error: string | null;
    }[];
    path: string | null;
  } | null;
}

export interface Backend {
  workspace(): Promise<Workspace>;
  chooseFiles(): Promise<string[]>;
  chooseLegacy(): Promise<string | null>;
  chooseExport(format: "csv" | "xlsx"): Promise<string | null>;
  preview(path: string): Promise<Preview>;
  import(files: FilePlan[]): Promise<number>;
  migrate(path: string): Promise<number>;
  search(query: Query, offset: number, limit: number): Promise<Results>;
  detail(id: number, revision: number): Promise<Row | null>;
  summary(query: Query, dimension: Dimension): Promise<Summary>;
  export(query: Query, path: string, columns: string[] | null): Promise<number>;
  job(id: number): Promise<Job>;
  cancel(id: number): Promise<void>;
}

const num = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 2 });
export const count = (n: number) => num.format(n);
// Format arbitrary-precision decimal strings without converting monetary totals to Number.
export function decimal(raw: string | null) {
  if (raw == null) return "Нет данных";
  const match = /^(-?)(\d+)(?:\.(\d+))?$/.exec(raw);
  if (!match) return raw;
  return (
    match[1] +
    match[2].replace(/\B(?=(\d{3})+(?!\d))/g, " ") +
    (match[3] ? "," + match[3] : "")
  );
}
export const messageOf = (err: unknown) =>
  err instanceof Error ? err.message : String(err);
export const basename = (path: string) => path.split(/[\\/]/).pop() || path;
export const PAGE_SIZE = 50;
export const operations: Record<Op, string> = {
  contains: "Содержит",
  eq: "Равно",
  prefix: "Начинается с",
  from: "От",
  to: "До",
  empty: "Пусто",
};
export function fieldName(field: string, columns: Column[]) {
  return (
    columns.find((column) => column.id === field)?.label ??
    roleLabels[field.slice(1) as Role] ??
    field
  );
}
