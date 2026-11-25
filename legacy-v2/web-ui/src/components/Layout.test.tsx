/** @vitest-environment jsdom */

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/client", () => ({
  api: { logout: vi.fn() },
}));

vi.mock("../lib/router", () => ({
  navigate: vi.fn(),
  useRoute: () => "search",
}));

vi.mock("../lib/i18n", () => ({
  useI18n: () => ({
    lang: "en",
    t: (key: string) =>
      ({
        appName: "Base Search",
        tagline: "Local data intelligence",
        nav_search: "Search",
        nav_analytics: "Analytics",
        nav_analyze: "Analyze",
        nav_data: "Data",
        nav_risk: "Price risk",
        nav_exports: "Exports",
        nav_columns: "Columns",
        nav_jobs: "Jobs",
        nav_settings: "Settings",
        common_menu: "Menu",
        common_close: "Close",
        common_sign_out: "Sign out",
        shell_skip_to_content: "Skip to content",
        shell_personal_workspace: "Personal workspace",
        shell_personal_no_sign_in: "No sign-in required",
        shell_personal_short: "Personal",
        shell_lan_active: "LAN mode active",
        theme_light: "Light theme",
        theme_dark: "Dark theme",
      })[key] ?? key,
  }),
}));

vi.mock("../state/store", () => ({
  useStore: () => ({
    activeJobs: 0,
    theme: "dark",
    toggleTheme: vi.fn(),
    status: { lan_exposed: false },
    auth: {
      required: false,
      authenticated: true,
      user: { username: "local-owner", role: "owner" },
    },
    refreshAuth: vi.fn(),
  }),
}));

afterEach(cleanup);

describe("Layout personal mode", () => {
  it("shows passwordless personal status without owner or sign-out noise", async () => {
    const { Layout } = await import("./Layout");
    const view = render(<Layout title="Search">Content</Layout>);

    expect(view.getByText("Personal workspace")).toBeTruthy();
    expect(view.getByText("No sign-in required")).toBeTruthy();
    expect(view.queryByText(/local-owner/i)).toBeNull();
    expect(view.queryByRole("button", { name: "Sign out" })).toBeNull();
  });
});
