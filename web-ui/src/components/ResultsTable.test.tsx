/** @vitest-environment jsdom */

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { FieldDto } from "../api/types";
import { ResultsTable, formatResultCell } from "./ResultsTable";

const numericField: FieldDto = {
  id: "source:value",
  label: "Value",
  kind: "number",
  source: { kind: "column", name: "value" },
  operators: ["Equals"],
};

afterEach(cleanup);

describe("ResultsTable numeric display", () => {
  it("removes floating-point artifacts while keeping the raw value available to actions", () => {
    expect(formatResultCell(numericField, "229.28400000000002")).toBe("229.284");

    const view = render(
      <ResultsTable
        fields={[numericField]}
        rows={[{ id: 1, values: ["229.28400000000002"] }]}
        onOpen={() => {}}
      />,
    );

    expect(view.getByText("229.284")).toBeTruthy();
    expect(view.queryByText("229.28400000000002")).toBeNull();
  });

  it("does not reformat code fields", () => {
    const codeField = { ...numericField, kind: "code" as const };
    expect(formatResultCell(codeField, "001234.00")).toBe("001234.00");
  });
});
