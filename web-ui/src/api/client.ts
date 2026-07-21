// Thin fetch wrapper around the canonical Base Search `/api/v2`. Every call returns parsed
// JSON or throws an `ApiError` carrying the backend's code + message.

import type {
  AccountInfo,
  AnalyticsEnvelope,
  AnalyticsScope,
  AuthState,
  CompanyProfile,
  CompareEnvelope,
  CompareSideRequest,
  CountResponse,
  DatabaseStats,
  EngineStatus,
  FixedSemanticField,
  ImportLogEntry,
  Job,
  PivotDim,
  PivotMetric,
  PivotResult,
  Query,
  RecordDto,
  ResultSort,
  SchemaResponse,
  SearchResponse,
  SemanticField,
  SessionUser,
  StatusResponse,
  SourceMappingProfile,
  SourceMappingProfileCollection,
  SourceMappingProfileUpsert,
  Undervaluation,
  WorkbookPeek,
} from "./types";

export class ApiError extends Error {
  code: string;
  status: number;
  constructor(code: string, message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
  }
}

const API_ROOT = "/api/v2";

export function apiUrl(path: string): string {
  if (path === API_ROOT || path.startsWith(`${API_ROOT}/`)) return path;
  if (path === "/api") return API_ROOT;
  if (path.startsWith("/api/")) return `${API_ROOT}${path.slice(4)}`;
  return `${API_ROOT}${path.startsWith("/") ? path : `/${path}`}`;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  const url = apiUrl(path);
  try {
    const requestInit = withRequestToken(init);
    response = await fetch(url, requestInit);
  } catch (err) {
    throw new ApiError(
      "network",
      "Cannot reach the Base Search server. Is it still running?",
      0,
    );
  }
  const text = await response.text();
  const body = text ? safeParse(text) : undefined;
  if (!response.ok) {
    // A session that expired mid-use: let the app re-check auth and show login.
    if (response.status === 401 && !url.startsWith(`${API_ROOT}/auth/`)) {
      window.dispatchEvent(new Event("bs-unauthorized"));
    }
    const detail = (body as { error?: { code?: string; message?: string } })
      ?.error;
    throw new ApiError(
      detail?.code ?? "error",
      detail?.message ?? `Request failed (${response.status})`,
      response.status,
    );
  }
  return body as T;
}

function withRequestToken(init?: RequestInit): RequestInit | undefined {
  const method = (init?.method ?? "GET").toUpperCase();
  if (!["POST", "PUT", "PATCH", "DELETE"].includes(method)) {
    return init;
  }
  const csrf = readCookie("bs_csrf");
  if (!csrf) return init;

  const headers = new Headers(init?.headers);
  headers.set("X-BS-CSRF", csrf);
  return { ...init, headers };
}

function readCookie(name: string): string | null {
  const prefix = `${name}=`;
  for (const part of document.cookie.split(";")) {
    const cookie = part.trim();
    if (cookie.startsWith(prefix)) {
      return cookie.slice(prefix.length);
    }
  }
  return null;
}

function safeParse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

function postJson<T>(path: string, payload: unknown): Promise<T> {
  return request<T>(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}

export const api = {
  status: () => request<StatusResponse>("/api/status"),
  schema: () => request<SchemaResponse>("/api/schema"),

  setColumnSemantic: (columnId: string, semantic: SemanticField | null) =>
    postJson<{ ok: boolean; columns: SchemaResponse["columns"] }>(
      `/api/columns/${encodeURIComponent(columnId)}/semantic`,
      { semantic },
    ),

  search: (
    query: Query,
    limit: number,
    offset: number,
    sort?: ResultSort | null,
  ) => postJson<SearchResponse>("/api/search", { query, limit, offset, sort }),

  count: (query: Query) => postJson<CountResponse>("/api/count", { query }),

  record: (id: number) => request<RecordDto>(`/api/records/${id}`),

  company: (edrpou: string, limit = 10) =>
    request<CompanyProfile>(
      `/api/company/${encodeURIComponent(edrpou)}?limit=${limit}`,
    ),

  undervaluation: (
    query: Query,
    threshold: number,
    minSamples: number,
    limit: number,
  ) =>
    postJson<Undervaluation>("/api/analytics/undervaluation", {
      query,
      threshold,
      min_samples: minSamples,
      limit,
    }),

  analytics: (
    query: Query,
    scope: AnalyticsScope | null,
    hsLevel: number,
    limit: number,
    engine: "auto" | "duckdb" | "sqlite" = "auto",
    full = false,
  ) => {
    if (full) {
      return postJson<AnalyticsEnvelope>("/api/analytics", {
        query,
        scope,
        hs_level: hsLevel,
        limit,
        engine,
        full: true,
      });
    }
    if (scope) {
      return postJson<AnalyticsEnvelope>("/api/analytics/section", {
        query,
        scope,
        hs_level: hsLevel,
        limit,
        engine,
      });
    }
    return postJson<AnalyticsEnvelope>("/api/analytics/overview", {
      query,
      limit,
      engine,
    });
  },

  compare: (
    left: CompareSideRequest,
    right: CompareSideRequest,
    scope: AnalyticsScope | null,
    hsLevel: number,
    limit: number,
    engine: "auto" | "duckdb" | "sqlite" = "auto",
  ) =>
    postJson<CompareEnvelope>("/api/compare", {
      left,
      right,
      scope,
      hs_level: hsLevel,
      limit,
      engine,
    }),

  engines: () => request<EngineStatus>("/api/engines"),

  pivot: (
    query: Query,
    rowDim: PivotDim,
    colDim: PivotDim,
    metric: PivotMetric,
    rows: number,
    cols: number,
  ) =>
    postJson<PivotResult>("/api/pivot", {
      query,
      row_dim: rowDim,
      col_dim: colDim,
      metric,
      rows,
      cols,
    }),

  importLog: (limit = 50) =>
    request<ImportLogEntry[]>(`/api/imports/log?limit=${limit}`),

  mappingProfiles: () =>
    request<SourceMappingProfileCollection>("/api/imports/profiles"),
  mappingProfile: (id: number) =>
    request<SourceMappingProfile>(`/api/imports/profiles/${id}`),
  suggestMappingProfiles: (signature: string) =>
    request<SourceMappingProfileCollection>(
      `/api/imports/profiles/suggest?signature=${encodeURIComponent(signature)}`,
    ),
  saveMappingProfile: (profile: SourceMappingProfileUpsert) =>
    postJson<SourceMappingProfile>("/api/imports/profiles", profile),
  deleteMappingProfile: (id: number) =>
    request<{ deleted: boolean }>(`/api/imports/profiles/${id}`, {
      method: "DELETE",
    }),

  uploadImport: (
    files: FileList | File[],
    selectedSheets?: string[],
    sheetSemantics?: Record<string, Record<number, SemanticField | null>>,
    sheetProfiles?: Record<string, number>,
    sheetFixedValues?: Record<
      string,
      Partial<Record<FixedSemanticField, string>>
    >,
  ) => {
    const form = new FormData();
    if (selectedSheets) {
      form.append("selected_sheets", JSON.stringify(selectedSheets));
    }
    if (sheetSemantics && Object.keys(sheetSemantics).length > 0) {
      form.append("sheet_semantics", JSON.stringify(sheetSemantics));
    }
    if (sheetProfiles && Object.keys(sheetProfiles).length > 0) {
      form.append("sheet_profiles", JSON.stringify(sheetProfiles));
    }
    if (sheetFixedValues && Object.keys(sheetFixedValues).length > 0) {
      form.append("sheet_fixed_values", JSON.stringify(sheetFixedValues));
    }
    for (const file of Array.from(files)) {
      form.append("files", file, file.name);
    }
    return request<Job>("/api/imports", { method: "POST", body: form });
  },

  peekImport: (file: File) => {
    const form = new FormData();
    form.append("files", file, file.name);
    return request<WorkbookPeek>("/api/imports/peek", {
      method: "POST",
      body: form,
    });
  },

  createExport: (
    query: Query,
    format: "csv" | "xlsx",
    filename?: string,
    fieldIds?: string[],
    sort?: ResultSort | null,
  ) =>
    postJson<Job>("/api/export", {
      query,
      format,
      filename,
      field_ids: fieldIds,
      sort: sort ?? undefined,
    }),

  jobs: () => request<{ jobs: Job[] }>("/api/jobs").then((r) => r.jobs),
  job: (id: number) => request<Job>(`/api/jobs/${id}`),
  cancelJob: (id: number) =>
    postJson<{ cancelled: boolean }>(`/api/jobs/${id}/cancel`, {}),

  me: () => request<AuthState>("/api/me"),
  login: (username: string, password: string) =>
    postJson<SessionUser>("/api/auth/login", { username, password }),
  logout: () => postJson<{ ok: boolean }>("/api/auth/logout", {}),
  accounts: () => request<AccountInfo[]>("/api/admin/users"),
  createAccount: (username: string, password: string, role: string) =>
    postJson<{ ok: boolean }>("/api/admin/users", {
      username,
      password,
      role,
    }),
  deleteAccount: (username: string) =>
    request<{ ok: boolean }>(
      `/api/admin/users/${encodeURIComponent(username)}`,
      { method: "DELETE" },
    ),

  stats: () => request<DatabaseStats>("/api/database/stats"),
  optimize: () => postJson<Job>("/api/database/optimize", {}),
  compact: () => postJson<Job>("/api/database/compact", {}),
  reindex: () => postJson<Job>("/api/database/reindex", {}),
  clear: () => postJson<Job>("/api/database/clear", {}),
  buildOlap: () => postJson<Job>("/api/admin/duckdb/rebuild", {}),
};
