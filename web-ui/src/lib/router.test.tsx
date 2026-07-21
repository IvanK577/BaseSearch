/** @vitest-environment jsdom */

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { navigate, parseRouteHash, updateRouteQuery, useRoute } from "./router";

afterEach(() => {
  cleanup();
  window.location.hash = "";
});

function RouteProbe() {
  const route = useRoute();
  return <output aria-label="route">{route}</output>;
}

describe("workspace router", () => {
  it("keeps a company deep link on the company screen", () => {
    window.location.hash = "#/company/12345678";
    const view = render(<RouteProbe />);
    expect(view.getByLabelText("route").textContent).toBe("company");
  });

  it("keeps a record drawer in the search URL", () => {
    window.location.hash = "#/search?record=42";

    expect(parseRouteHash(window.location.hash).query.get("record")).toBe("42");

    updateRouteQuery("record", null);
    expect(window.location.hash).toBe("#/search");
  });

  it("carries the applied query between workspace screens", () => {
    window.location.hash = "#/search?q=%7B%22text%22%3A%22coffee%22%7D&record=42";

    navigate("analytics");

    const location = parseRouteHash(window.location.hash);
    expect(location.route).toBe("analytics");
    expect(location.query.get("q")).toBe('{"text":"coffee"}');
    expect(location.query.has("record")).toBe(false);
  });
});
