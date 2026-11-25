/** @vitest-environment jsdom */

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const accounts = vi.fn();
const stats = vi.fn();
const engines = vi.fn();
const createAccount = vi.fn();
const store = {
  status: {
    version: "2.0.0",
    db_path: "audit.db",
    total_rows: 0,
    unindexed_rows: 0,
    has_data: false,
    has_shape: false,
    lan_exposed: false,
    storage: {
      database_bytes: 0,
      wal_bytes: 0,
      shm_bytes: 0,
      freelist_pages: 0,
      freelist_bytes: 0,
      total_file_bytes: 0,
    },
    extra_headers: [],
  },
  theme: "dark" as const,
  toggleTheme: vi.fn(),
  toast: vi.fn(),
  refreshJobs: vi.fn(),
  isAdmin: true,
  auth: {
    required: false,
    authenticated: true,
    user: { username: "local-owner", role: "owner" as const },
  },
};

vi.mock("../api/client", () => ({
  ApiError: class ApiError extends Error {},
  api: {
    stats,
    engines,
    accounts,
    createAccount,
    deleteAccount: vi.fn(),
    optimize: vi.fn(),
    reindex: vi.fn(),
    compact: vi.fn(),
    buildOlap: vi.fn(),
    clear: vi.fn(),
  },
}));

vi.mock("../state/store", () => ({
  useStore: () => store,
}));

beforeEach(() => {
  stats.mockResolvedValue(null);
  engines.mockResolvedValue({ duckdb_available: false });
  accounts.mockResolvedValue([]);
  store.status.lan_exposed = false;
  store.auth.required = false;
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.clearAllMocks();
});

describe("Settings workspace mode", () => {
  it("hides account management in passwordless personal mode", async () => {
    const { SettingsPage } = await import("./Settings");
    const view = render(<SettingsPage />);

    expect(view.getByText("Personal workspace")).toBeTruthy();
    expect(view.getByText("No account or password required")).toBeTruthy();
    expect(view.queryByText("Accounts")).toBeNull();
    expect(accounts).not.toHaveBeenCalled();
  });

  it("keeps account management available in LAN mode", async () => {
    store.status.lan_exposed = true;
    store.auth.required = true;
    const { SettingsPage } = await import("./Settings");
    const view = render(<SettingsPage />);

    expect(await view.findByText("Accounts")).toBeTruthy();
    expect(accounts).toHaveBeenCalledOnce();
  });

  it("blocks an existing account name instead of sending an ambiguous create", async () => {
    store.status.lan_exposed = true;
    store.auth.required = true;
    accounts.mockResolvedValue([
      { username: "Alice", role: "viewer", created_at: "2026-07-21" },
    ]);
    const { SettingsPage } = await import("./Settings");
    const view = render(<SettingsPage />);

    expect(await view.findByText("Alice")).toBeTruthy();
    fireEvent.change(view.getByLabelText("Username"), { target: { value: "alice" } });

    expect(
      view.getByText(
        "An account named Alice already exists. Choose a different username.",
      ),
    ).toBeTruthy();
    expect((view.getByRole("button", { name: "Add account" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(createAccount).not.toHaveBeenCalled();
  });
});
