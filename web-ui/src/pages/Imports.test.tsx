/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  SourceMappingProfile,
  SourceMappingProfileCollection,
  WorkbookPeek,
} from "../api/types";

const importLog = vi.fn();
const mappingProfiles = vi.fn();
const peekImport = vi.fn();
const uploadImport = vi.fn();
const saveMappingProfile = vi.fn();
const deleteMappingProfile = vi.fn();
const refreshJobs = vi.fn();
const toast = vi.fn();

vi.mock("../api/client", () => ({
  ApiError: class ApiError extends Error {
    code: string;
    status: number;
    constructor(code: string, message: string, status: number) {
      super(message);
      this.code = code;
      this.status = status;
    }
  },
  api: {
    importLog,
    mappingProfiles,
    peekImport,
    uploadImport,
    saveMappingProfile,
    deleteMappingProfile,
  },
}));

vi.mock("../state/store", () => ({
  useStore: () => ({
    jobs: [],
    refreshJobs,
    toast,
    canEditData: true,
  }),
}));

const profile: SourceMappingProfile = {
  id: 7,
  name: "Reusable source",
  signature: `smp1:2:${"a".repeat(64)}`,
  mapping: ["Recipient", null],
  fixed_values: { Currency: "USD", WeightUnit: "kg" },
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

const profileCollection: SourceMappingProfileCollection = {
  profiles: [profile],
  ignored_corrupt_rows: [],
};

const peek: WorkbookPeek = {
  sheets: [
    {
      name: "Data",
      rows: 2,
      cols: 2,
      header_row: 1,
      layout: "tabular",
      signature: profile.signature,
      profile_suggestions: profileCollection,
      columns: [
        {
          index: 0,
          id: "company",
          header: "Company",
          sample: "ACME",
          role: "Text",
          semantic: "Sender",
        },
        {
          index: 1,
          id: "amount",
          header: "Amount",
          sample: "1250",
          role: "Number",
          semantic: "Value",
        },
      ],
    },
  ],
};

function chooseSingleFile(container: HTMLElement) {
  const file = new File(["workbook"], "source.xlsx", {
    type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  });
  const input = container.querySelector<HTMLInputElement>('input[type="file"][multiple]');
  if (!input) throw new Error("main import file input not found");
  fireEvent.change(input, { target: { files: [file] } });
  return file;
}

beforeEach(() => {
  importLog.mockResolvedValue([]);
  mappingProfiles.mockResolvedValue(profileCollection);
  peekImport.mockResolvedValue(peek);
  uploadImport.mockResolvedValue({ id: 21 });
  saveMappingProfile.mockImplementation((request) =>
    Promise.resolve({
      ...profile,
      ...request,
      id: 9,
      created_at: profile.created_at,
      updated_at: profile.updated_at,
    }),
  );
  deleteMappingProfile.mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.clearAllMocks();
});

describe("ImportsPage source mapping profiles", () => {
  it("applies an exact suggestion while preserving explicit mapping and fixed-value overrides", async () => {
    const { ImportsPage } = await import("./Imports");
    const view = render(<ImportsPage />);
    const file = chooseSingleFile(view.container);

    const profileSelect = await view.findByLabelText("Choose a saved mapping");
    fireEvent.change(profileSelect, { target: { value: "7" } });

    const selects = view.getAllByRole("combobox");
    fireEvent.change(selects[1], { target: { value: "Description" } });
    fireEvent.click(view.getByText("Mapping defaults"));
    fireEvent.change(view.getByLabelText("Currency"), {
      target: { value: "EUR" },
    });
    fireEvent.click(view.getByRole("button", { name: "Import selected" }));

    await waitFor(() => expect(uploadImport).toHaveBeenCalledOnce());
    expect(uploadImport).toHaveBeenCalledWith(
      [file],
      ["Data"],
      { Data: { 0: "Description" } },
      { Data: 7 },
      { Data: { Currency: "EUR", WeightUnit: "kg" } },
    );
  });

  it("saves the effective mapping and manages deletion without expanding the main workflow", async () => {
    const { ImportsPage } = await import("./Imports");
    const view = render(<ImportsPage />);
    chooseSingleFile(view.container);

    await view.findByText("Exact saved mapping found");
    fireEvent.click(view.getByText("Mapping defaults"));
    fireEvent.change(view.getByLabelText("Currency"), {
      target: { value: "USD" },
    });
    fireEvent.change(view.getByLabelText("Profile name"), {
      target: { value: "Supplier export" },
    });
    fireEvent.click(view.getByRole("button", { name: "Save mapping" }));

    await waitFor(() => expect(saveMappingProfile).toHaveBeenCalledOnce());
    expect(saveMappingProfile).toHaveBeenCalledWith({
      name: "Supplier export",
      signature: profile.signature,
      mapping: ["Sender", "Value"],
      fixed_values: { Currency: "USD" },
    });

    fireEvent.click(view.getByText("Saved mappings (2)"));
    fireEvent.click(view.getByRole("button", { name: "Delete Reusable source" }));
    await waitFor(() => expect(deleteMappingProfile).toHaveBeenCalledWith(7));
  });

  it("localizes a profile signature mismatch instead of exposing the server text", async () => {
    const { ApiError } = await import("../api/client");
    uploadImport.mockRejectedValueOnce(
      new ApiError("profile_signature_mismatch", "raw server message", 400),
    );
    const { ImportsPage } = await import("./Imports");
    const view = render(<ImportsPage />);
    chooseSingleFile(view.container);

    const profileSelect = await view.findByLabelText("Choose a saved mapping");
    fireEvent.change(profileSelect, { target: { value: "7" } });
    fireEvent.click(view.getByRole("button", { name: "Import selected" }));

    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "This saved mapping no longer matches the sheet. Preview the file and choose again.",
        "error",
      ),
    );
  });
});
