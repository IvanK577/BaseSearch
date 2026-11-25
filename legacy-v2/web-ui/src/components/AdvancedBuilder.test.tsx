/** @vitest-environment jsdom */

import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { FieldDto, QueryExpr } from "../api/types";
import { AdvancedBuilder } from "./AdvancedBuilder";

const fields: FieldDto[] = [
  {
    id: "description",
    label: "Description",
    kind: "text",
    source: { kind: "column", name: "description" },
    operators: ["Contains", "Equals"],
  },
  {
    id: "country",
    label: "Country",
    kind: "country",
    source: { kind: "extra", header: "Country" },
    operators: ["Equals", "IsAnyOf"],
  },
];

afterEach(cleanup);

describe("AdvancedBuilder", () => {
  it("does not erase an existing query merely because the editor was opened", async () => {
    const onChange = vi.fn();
    render(<AdvancedBuilder fields={fields} onChange={onChange} />);

    await waitFor(() => expect(onChange).not.toHaveBeenCalled());
  });

  it("loads a saved nested OR and NOT expression without flattening it", () => {
    const value: QueryExpr = {
      Group: {
        op: "Or",
        negated: false,
        children: [
          {
            Condition: {
              field: { Column: "description" },
              op: "Contains",
              value: { Single: "coffee" },
              negated: false,
            },
          },
          {
            Condition: {
              field: { Extra: "Country" },
              op: "Equals",
              value: { Single: "RU" },
              negated: true,
            },
          },
        ],
      },
    };
    const onChange = vi.fn();
    const view = render(
      <AdvancedBuilder fields={fields} onChange={onChange} value={value} />,
    );

    expect(view.getByDisplayValue("coffee")).toBeTruthy();
    expect(view.getByDisplayValue("RU")).toBeTruthy();
    expect(view.getByRole("button", { name: "OR", pressed: true })).toBeTruthy();
    expect(
      (view.getByRole("checkbox", {
        name: "Exclude condition 2",
      }) as HTMLInputElement).checked,
    ).toBe(true);
    expect(onChange).not.toHaveBeenCalled();
  });
});
