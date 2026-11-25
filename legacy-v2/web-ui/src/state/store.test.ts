/** @vitest-environment jsdom */

import { createElement } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  me: vi.fn(),
  status: vi.fn(),
  jobs: vi.fn(),
}));

vi.mock("../api/client", () => ({
  api: apiMocks,
}));

import { capabilitiesForAuth, StoreProvider, useStore } from "./store";

beforeEach(() => {
  apiMocks.me.mockReset();
  apiMocks.status.mockReset().mockResolvedValue(null);
  apiMocks.jobs.mockReset().mockResolvedValue([]);
  localStorage.clear();
});

afterEach(cleanup);

describe("workspace role capabilities", () => {
  it("matches owner, admin, editor, viewer and personal mode", () => {
    expect(capabilitiesForAuth(null)).toEqual({ isAdmin: false, canEditData: false });
    expect(
      capabilitiesForAuth({ required: false, authenticated: false }),
    ).toEqual({ isAdmin: true, canEditData: true });
    expect(
      capabilitiesForAuth({
        required: true,
        authenticated: true,
        user: { username: "owner", role: "owner" },
      }),
    ).toEqual({ isAdmin: true, canEditData: true });
    expect(
      capabilitiesForAuth({
        required: true,
        authenticated: true,
        user: { username: "editor", role: "editor" },
      }),
    ).toEqual({ isAdmin: false, canEditData: true });
    expect(
      capabilitiesForAuth({
        required: true,
        authenticated: true,
        user: { username: "viewer", role: "viewer" },
      }),
    ).toEqual({ isAdmin: false, canEditData: false });
  });

  it("keeps capabilities closed until /me succeeds and exposes a retryable error", async () => {
    apiMocks.me.mockRejectedValueOnce(new Error("temporary auth failure"));

    function AuthProbe() {
      const state = useStore();
      return createElement(
        "button",
        { type: "button", onClick: state.refreshAuth },
        `${state.authReadiness}|${state.isAdmin}|${state.canEditData}|${state.needsLogin}|${state.authError ?? "none"}`,
      );
    }

    render(createElement(StoreProvider, null, createElement(AuthProbe)));

    expect(screen.getByRole("button").textContent).toContain("unknown|false|false|false");
    await waitFor(() =>
      expect(screen.getByRole("button").textContent).toBe(
        "error|false|false|false|temporary auth failure",
      ),
    );

    apiMocks.me.mockResolvedValueOnce({ required: false, authenticated: false });
    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByRole("button").textContent).toContain("unknown|false|false|false");
    await waitFor(() =>
      expect(screen.getByRole("button").textContent).toBe("ready|true|true|false|none"),
    );
  });
});
