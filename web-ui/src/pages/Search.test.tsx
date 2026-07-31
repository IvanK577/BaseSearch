/** @vitest-environment jsdom */

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SearchResponse } from "../api/types";
import { QueryProvider } from "../state/query";

const apiMocks = vi.hoisted(() => ({
  schema: vi.fn(),
  search: vi.fn(),
  count: vi.fn(),
}));

vi.mock("../api/client", () => ({
  ApiError: class ApiError extends Error {},
  api: apiMocks,
}));

vi.mock("../state/store", () => ({
  useStore: () => ({
    openCompany: vi.fn(),
    toast: vi.fn(),
    status: { has_data: true },
  }),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

function response(value: string, id: number): SearchResponse {
  return {
    fields: [
      {
        id: "description",
        label: "Description",
        kind: "text",
        source: { kind: "column", name: "description" },
        operators: ["Contains", "Equals"],
      },
    ],
    rows: [{ id, values: [value] }],
    offset: 0,
    limit: 50,
    has_next: false,
    total: 1,
    snapshot: 99,
  };
}

beforeEach(() => {
  localStorage.clear();
  window.location.hash = "#/search";
  apiMocks.schema.mockReset().mockResolvedValue({
    search_fields: [],
    result_fields: [],
    columns: [],
    has_shape: false,
    total_rows: 0,
  });
  apiMocks.search.mockReset();
  apiMocks.count.mockReset();
});

afterEach(cleanup);

describe("Search request ordering", () => {
  it("keeps the newest result when requests finish out of order", async () => {
    const firstSearch = deferred<SearchResponse>();
    const secondSearch = deferred<SearchResponse>();
    apiMocks.search
      .mockReturnValueOnce(firstSearch.promise)
      .mockReturnValueOnce(secondSearch.promise);
    const { SearchPage } = await import("./Search");
    const view = render(
      <QueryProvider>
        <SearchPage />
      </QueryProvider>,
    );
    const input = view.getByPlaceholderText(
      "Search description, company, product code, trademark…",
    );

    fireEvent.change(input, { target: { value: "first" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.change(input, { target: { value: "second" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await act(async () => {
      secondSearch.resolve(response("second result", 2));
    });
    expect(await view.findByText("second result")).toBeTruthy();

    await act(async () => {
      firstSearch.resolve(response("stale result", 1));
    });
    expect(view.queryByText("stale result")).toBeNull();
    expect(view.getByText("second result")).toBeTruthy();
    expect(apiMocks.search.mock.calls[0][0].text).toBe("first");
    expect(apiMocks.search.mock.calls[1][0].text).toBe("second");
    // The total rides along on the search response. A separate /api/count
    // would re-run the most expensive part of the same query for nothing.
    expect(apiMocks.count).not.toHaveBeenCalled();
  });

  it("invalidates an in-flight request when Reset clears the workspace", async () => {
    const pendingSearch = deferred<SearchResponse>();
    apiMocks.search.mockReturnValueOnce(pendingSearch.promise);
    const { SearchPage } = await import("./Search");
    const view = render(
      <QueryProvider>
        <SearchPage />
      </QueryProvider>,
    );
    const input = view.getByPlaceholderText(
      "Search description, company, product code, trademark…",
    );

    fireEvent.change(input, { target: { value: "pending" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.click(view.getByRole("button", { name: "Reset" }));

    await act(async () => {
      pendingSearch.resolve(response("should stay hidden", 3));
    });
    expect(view.queryByText("should stay hidden")).toBeNull();
    expect(
      view.getByText("Type a query or add a filter to search the database."),
    ).toBeTruthy();
  });
});
