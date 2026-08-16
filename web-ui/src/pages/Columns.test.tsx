/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SchemaResponse } from "../api/types";

const schema = vi.fn();
const setFixedValues = vi.fn();
const toast = vi.fn();

vi.mock("../api/client", () => ({
  ApiError: class ApiError extends Error {},
  api: { schema, setFixedValues, setColumnSemantic: vi.fn() },
}));

vi.mock("../state/store", () => ({
  useStore: () => ({ toast, canEditData: true }),
}));

const response: SchemaResponse = {
  search_fields: [],
  result_fields: [],
  columns: [
    {
      id: "value",
      header: "ФВ вал.контр",
      source_index: 0,
      role: "Money",
      semantic: "Value",
      storage: { SchemaColumn: "currency_control_value" },
    },
  ],
  has_shape: true,
  total_rows: 2,
  fixed_currency: null,
  fixed_weight_unit: null,
};

beforeEach(() => {
  schema.mockResolvedValue(response);
  setFixedValues.mockResolvedValue({
    ok: true,
    fixed_currency: "USD",
    fixed_weight_unit: "kg",
    changed_schemas: 1,
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Columns page currency control", () => {
  it("pins a currency the source never states", async () => {
    const { ColumnsPage } = await import("./Columns");
    const view = render(<ColumnsPage />);

    const currency = (await view.findByLabelText("Currency")) as HTMLInputElement;
    const weight = view.getByLabelText("Weight unit") as HTMLInputElement;
    const apply = view.getByRole("button", { name: "Apply" }) as HTMLButtonElement;

    // Nothing pinned yet, so there is nothing to apply.
    expect(currency.value).toBe("");
    expect(apply.disabled).toBe(true);

    // Whitespace around a pasted code must not reach the server, and an empty
    // field must clear rather than store "".
    fireEvent.change(currency, { target: { value: "  usd  " } });
    expect(apply.disabled).toBe(false);
    fireEvent.click(apply);

    await waitFor(() => expect(setFixedValues).toHaveBeenCalledWith("usd", null));
    await waitFor(() => expect(currency.value).toBe("USD"));
    expect(weight.value).toBe("kg");
    expect(toast).toHaveBeenCalledWith("Applied to every imported table", "success");

    // The saved state is the new baseline: re-applying the same values is off.
    expect((view.getByRole("button", { name: "Apply" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it("reports a rejected value instead of pretending it was saved", async () => {
    const { ApiError } = await import("../api/client");
    setFixedValues.mockRejectedValue(
      new (ApiError as unknown as new (message: string) => Error)(
        "Fixed Currency value must be at most 32 characters.",
      ),
    );
    const { ColumnsPage } = await import("./Columns");
    const view = render(<ColumnsPage />);

    const currency = (await view.findByLabelText("Currency")) as HTMLInputElement;
    fireEvent.change(currency, { target: { value: "X".repeat(33) } });
    fireEvent.click(view.getByRole("button", { name: "Apply" }));

    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "Fixed Currency value must be at most 32 characters.",
        "error",
      ),
    );
  });
});
