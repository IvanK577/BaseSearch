/** @vitest-environment jsdom */

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ErrorBoundary } from "./ErrorBoundary";

afterEach(cleanup);

function BrokenView(): never {
  throw new Error("render failed");
}

describe("ErrorBoundary", () => {
  it("replaces a broken screen with a recoverable error state", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const reload = vi.fn();
    const view = render(
      <ErrorBoundary onReload={reload}>
        <BrokenView />
      </ErrorBoundary>,
    );

    expect(view.getByRole("alert").textContent).toContain("could not display");
    fireEvent.click(view.getByRole("button", { name: "Reload Base Search" }));
    expect(reload).toHaveBeenCalledOnce();
    consoleError.mockRestore();
  });
});
