import { describe, expect, it } from "vitest";

import { capabilitiesForAuth } from "./store";

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
});
