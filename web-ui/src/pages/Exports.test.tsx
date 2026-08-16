/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { emptyQuery, type SchemaResponse } from "../api/types";

const createExport = vi.fn();
const schema = vi.fn();
const count = vi.fn();
const toast = vi.fn();
const appliedQuery = { ...emptyQuery(), text: "Apple" };

vi.mock("../api/client", () => ({
  ApiError: class ApiError extends Error {
    code: string;
    status: number;
    constructor(code: string, message: string, status: number) {
      super(message);
      this.code = code;
      this.status = status;
    }
  },
  apiUrl: (path: string) => path.replace(/^\/api(?=\/|$)/, "/api/v2"),
  api: { createExport, schema, count },
}));
vi.mock("../state/query", () => ({
  useQueryStore: () => ({ query: appliedQuery }),
}));
vi.mock("../state/store", () => ({
  useStore: () => ({ jobs: [], refreshJobs: vi.fn(), toast }),
}));
vi.mock("../lib/router", () => ({ navigate: vi.fn() }));

const response: SchemaResponse = {
  search_fields: [],
  result_fields: [
    {
      id: "alpha",
      label: "Alpha",
      kind: "text",
      source: { kind: "column", name: "alpha" },
      operators: [],
    },
    {
      id: "beta",
      label: "Beta",
      kind: "number",
      source: { kind: "column", name: "beta" },
      operators: [],
    },
  ],
  columns: [],
  has_shape: false,
  total_rows: 12,
  fixed_currency: null,
  fixed_weight_unit: null,
};

beforeEach(() => {
  schema.mockResolvedValue(response);
  count.mockResolvedValue({ total: 12 });
  createExport.mockResolvedValue({ id: 7 });
  localStorage.setItem(
    "base-search.result-sort.v1",
    JSON.stringify({ field: "beta", descending: true }),
  );
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.clearAllMocks();
});

describe("ExportsPage", () => {
  it("confirms the applied query, ordered fields and active result sort before starting", async () => {
    const { ExportsPage } = await import("./Exports");
    const view = render(<ExportsPage />);

    await view.findByText("Alpha");
    expect(view.getByText("Text: “Apple”")).toBeTruthy();
    expect(view.getByText("Sort: Beta · descending")).toBeTruthy();

    fireEvent.click(view.getByRole("button", { name: "Move Beta up" }));
    fireEvent.click(view.getByRole("button", { name: "Review export" }));
    expect(view.getByText("Confirm export")).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Start export" }));

    await waitFor(() => expect(createExport).toHaveBeenCalledOnce());
    expect(createExport).toHaveBeenCalledWith(
      expect.objectContaining({ text: "Apple" }),
      "csv",
      undefined,
      ["beta", "alpha"],
      { field: "beta", descending: true },
    );
  });

  it("shows a localized message for a known export API error", async () => {
    const { ApiError } = await import("../api/client");
    createExport.mockRejectedValueOnce(
      new ApiError("export_error_unknown_field", "Server fallback", 400),
    );
    const { ExportsPage } = await import("./Exports");
    const view = render(<ExportsPage />);

    await view.findByText("Alpha");
    fireEvent.click(view.getByRole("button", { name: "Review export" }));
    fireEvent.click(view.getByRole("button", { name: "Start export" }));

    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "One of the selected columns is no longer available.",
        "error",
      ),
    );
  });
});
