/** @vitest-environment jsdom */

import { afterEach, describe, expect, it, vi } from "vitest";

import { api, apiUrl } from "./client";
import { emptyQuery, type ResultSort } from "./types";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("export API", () => {
  it("sends the applied query, ordered fields and active sort", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          id: 1,
          kind: "export",
          status: "queued",
          title: "Export",
          progress: { phase: "", done: 0, total: 0, percent: 0 },
          cancellable: true,
          created_ms: 1,
          updated_ms: 1,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const sort: ResultSort = { field: "amount", descending: true };

    await api.createExport(
      emptyQuery(),
      "csv",
      "report",
      ["amount", "company"],
      sort,
    );

    expect(fetchMock).toHaveBeenCalledOnce();
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/v2/export");
    expect(JSON.parse(String(init.body))).toMatchObject({
      format: "csv",
      filename: "report",
      field_ids: ["amount", "company"],
      sort,
      query: { record_scope: "canonical" },
    });
  });

  it("normalizes legacy and relative API paths to the canonical v2 prefix", () => {
    expect(apiUrl("/api/exports/7/download")).toBe("/api/v2/exports/7/download");
    expect(apiUrl("/api/v2/schema")).toBe("/api/v2/schema");
    expect(apiUrl("jobs")).toBe("/api/v2/jobs");
  });

  it("dispatches unauthorized handling for protected v2 requests but not login", async () => {
    const fetchMock = vi.fn().mockImplementation(() =>
      Promise.resolve(new Response(
        JSON.stringify({ error: { code: "unauthorized", message: "Sign in." } }),
        { status: 401, headers: { "Content-Type": "application/json" } },
      )),
    );
    vi.stubGlobal("fetch", fetchMock);
    const unauthorized = vi.fn();
    window.addEventListener("bs-unauthorized", unauthorized);

    await expect(api.status()).rejects.toMatchObject({ code: "unauthorized" });
    expect(fetchMock.mock.calls[0][0]).toBe("/api/v2/status");
    expect(unauthorized).toHaveBeenCalledOnce();

    await expect(api.login("owner", "password")).rejects.toMatchObject({
      code: "unauthorized",
    });
    expect(fetchMock.mock.calls[1][0]).toBe("/api/v2/auth/login");
    expect(unauthorized).toHaveBeenCalledOnce();

    window.removeEventListener("bs-unauthorized", unauthorized);
  });
});

describe("canonical v2 contract", () => {
  it("uses canonical analytics, account, export and maintenance routes", async () => {
    const fetchMock = vi.fn().mockImplementation(() =>
      Promise.resolve(
        new Response(JSON.stringify({}), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await api.me();
    await api.analytics(emptyQuery(), null, 10, 10, "sqlite");
    await api.analytics(emptyQuery(), "companies", 10, 10, "sqlite");
    await api.pivot(emptyQuery(), "recipient", "year", "rows", 10, 10);
    await api.accounts();
    await api.createAccount("analyst", "password", "viewer");
    await api.deleteAccount("analyst");
    await api.buildOlap();

    expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
      "/api/v2/me",
      "/api/v2/analytics/overview",
      "/api/v2/analytics/section",
      "/api/v2/pivot",
      "/api/v2/admin/users",
      "/api/v2/admin/users",
      "/api/v2/admin/users/analyst",
      "/api/v2/admin/duckdb/rebuild",
    ]);
  });

  it("sends two typed queries and explicit labels to compare", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          left: { label: "Current", query: emptyQuery(), engine: "sqlite", data: {} },
          right: { label: "Previous", query: emptyQuery(), engine: "sqlite", data: {} },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await api.compare(
      { label: "Current", query: emptyQuery() },
      { label: "Previous", query: { ...emptyQuery(), text: "Apple" } },
      null,
      10,
      10,
      "sqlite",
    );

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/v2/compare");
    expect(JSON.parse(String(init.body))).toMatchObject({
      left: { label: "Current", query: { record_scope: "canonical" } },
      right: {
        label: "Previous",
        query: { text: "Apple", record_scope: "canonical" },
      },
      scope: null,
      hs_level: 10,
      limit: 10,
      engine: "sqlite",
    });
  });
});

describe("source mapping profile API", () => {
  it("uses canonical profile routes and includes explicit import selections", async () => {
    const fetchMock = vi.fn().mockImplementation(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({ profiles: [], ignored_corrupt_rows: [] }),
          {
            status: 200,
            headers: { "Content-Type": "application/json" },
          },
        ),
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await api.mappingProfiles();
    expect(fetchMock.mock.calls[0][0]).toBe("/api/v2/imports/profiles");

    const file = new File(["Alpha,Beta\nACME,1\n"], "source.csv", {
      type: "text/csv",
    });
    await api.uploadImport(
      [file],
      ["source.csv"],
      { "source.csv": { 0: "Recipient" } },
      { "source.csv": 7 },
      { "source.csv": { Currency: "USD", WeightUnit: "kg" } },
    );
    const [path, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(path).toBe("/api/v2/imports");
    const form = init.body as FormData;
    expect(JSON.parse(String(form.get("sheet_profiles")))).toEqual({
      "source.csv": 7,
    });
    expect(JSON.parse(String(form.get("sheet_fixed_values")))).toEqual({
      "source.csv": { Currency: "USD", WeightUnit: "kg" },
    });
    expect(JSON.parse(String(form.get("sheet_semantics")))).toEqual({
      "source.csv": { 0: "Recipient" },
    });
  });
});
