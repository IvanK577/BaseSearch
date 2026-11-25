/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const login = vi.fn();
const refreshAuth = vi.fn();

class MockApiError extends Error {
  code: string;
  status: number;

  constructor(code: string, message: string, status: number) {
    super(message);
    this.code = code;
    this.status = status;
  }
}

vi.mock("../api/client", () => ({
  ApiError: MockApiError,
  api: { login },
}));

vi.mock("../state/store", () => ({
  useStore: () => ({ refreshAuth }),
}));

beforeEach(() => {
  login.mockReset();
  refreshAuth.mockReset();
  localStorage.clear();
});

afterEach(cleanup);

describe("LAN sign in", () => {
  it("renders trusted-workspace context with accessible credential fields", async () => {
    const { LoginScreen } = await import("./Login");
    const view = render(<LoginScreen />);
    const username = view.getByLabelText("Username") as HTMLInputElement;
    const password = view.getByLabelText("Password") as HTMLInputElement;

    expect(view.getByText("Trusted LAN workspace")).toBeTruthy();
    expect(document.activeElement).toBe(username);
    expect(username.autocomplete).toBe("username");
    expect(password.autocomplete).toBe("current-password");
  });

  it("localizes busy and invalid-credential states and exposes the error as an alert", async () => {
    const { LoginScreen } = await import("./Login");
    login.mockRejectedValueOnce(
      new MockApiError("invalid_credentials", "backend detail", 401),
    );
    const view = render(<LoginScreen />);

    fireEvent.change(view.getByLabelText("Username"), { target: { value: "owner" } });
    fireEvent.change(view.getByLabelText("Password"), { target: { value: "secret" } });
    fireEvent.submit(view.getByRole("button", { name: "Sign in" }).closest("form")!);

    expect(view.getByText("Signing in...")).toBeTruthy();
    const alert = await view.findByRole("alert");
    expect(alert.textContent).toContain("The username or password is incorrect.");
    expect(view.getByLabelText("Username").getAttribute("aria-invalid")).toBe("true");
    expect(refreshAuth).not.toHaveBeenCalled();
    await waitFor(() =>
      expect((view.getByRole("button", { name: "Sign in" }) as HTMLButtonElement).disabled).toBe(
        false,
      ),
    );
  });
});
