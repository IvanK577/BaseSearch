import type { UserRole } from "../api/types";
import type { MessageKey, Translate } from "./i18n";

const ROLE_KEYS: Record<UserRole, MessageKey> = {
  owner: "role_owner",
  admin: "role_admin",
  editor: "role_editor",
  viewer: "role_viewer",
};

export function roleLabel(t: Translate, role: UserRole): string {
  return t(ROLE_KEYS[role]);
}
