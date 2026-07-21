/** @vitest-environment jsdom */

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { emptyQuery, type QueryExpr } from "../api/types";
import { QueryProvider, useQueryStore } from "./query";

const first: QueryExpr = {
  Condition: {
    field: { Extra: "Brand" },
    op: "Equals",
    value: { Single: "Apple" },
    negated: false,
  },
};
const second: QueryExpr = {
  Condition: {
    field: { Extra: "Date" },
    op: "Range",
    value: { Range: { from: "2024-01-01", to: "2024-01-31" } },
    negated: false,
  },
};

afterEach(() => {
  cleanup();
  window.location.hash = "";
  window.localStorage.clear();
});

function Probe() {
  const { query, setQuery, applyDrilldown, undo, canUndo } = useQueryStore();
  return (
    <>
      <output aria-label="query">{JSON.stringify(query.advanced ?? null)}</output>
      <output aria-label="undo">{String(canUndo)}</output>
      <button onClick={() => setQuery({ ...emptyQuery(), advanced: first })}>seed</button>
      <button onClick={() => applyDrilldown(second)}>drill</button>
      <button onClick={undo}>undo</button>
    </>
  );
}

describe("applied query history", () => {
  it("adds a drill-down without deleting the previous tree and can undo it", () => {
    const view = render(
      <QueryProvider>
        <Probe />
      </QueryProvider>,
    );
    fireEvent.click(view.getByText("seed"));
    fireEvent.click(view.getByText("drill"));

    expect(JSON.parse(view.getByLabelText("query").textContent ?? "null")).toEqual({
      Group: { op: "And", negated: false, children: [first, second] },
    });
    expect(view.getByLabelText("undo").textContent).toBe("true");

    fireEvent.click(view.getByText("undo"));
    expect(JSON.parse(view.getByLabelText("query").textContent ?? "null")).toEqual(first);
  });
});
