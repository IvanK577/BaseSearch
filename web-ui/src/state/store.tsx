// Global app store: workspace status, background jobs (polled), toasts, and
// the theme. One small context keeps every page in sync with the server.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { api } from "../api/client";
import type { AuthState, Job, StatusResponse } from "../api/types";
import { navigate } from "../lib/router";

export type ToastKind = "info" | "success" | "error";
export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

type Theme = "dark" | "light";

interface StoreValue {
  status: StatusResponse | null;
  statusError: string | null;
  refreshStatus: () => void;
  jobs: Job[];
  activeJobs: number;
  refreshJobs: () => void;
  toasts: Toast[];
  toast: (message: string, kind?: ToastKind) => void;
  dismissToast: (id: number) => void;
  theme: Theme;
  toggleTheme: () => void;
  companyEdrpou: string | null;
  openCompany: (edrpou: string) => void;
  auth: AuthState | null;
  refreshAuth: () => void;
  /// True when this server requires sign-in and the user is not signed in.
  needsLogin: boolean;
  /// True when the user can perform admin actions (loopback owner or admin role).
  isAdmin: boolean;
  /// True when the user can import data and edit semantic mappings.
  canEditData: boolean;
}

const StoreContext = createContext<StoreValue | null>(null);

export function capabilitiesForAuth(auth: AuthState | null): {
  isAdmin: boolean;
  canEditData: boolean;
} {
  if (!auth) return { isAdmin: false, canEditData: false };
  if (!auth.required) return { isAdmin: true, canEditData: true };
  const role = auth.user?.role;
  return {
    isAdmin: role === "owner" || role === "admin",
    canEditData: role === "owner" || role === "admin" || role === "editor",
  };
}

function initialTheme(): Theme {
  return localStorage.getItem("bs-theme") === "light" ? "light" : "dark";
}

export function StoreProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [companyEdrpou, setCompanyEdrpou] = useState<string | null>(null);
  const [auth, setAuth] = useState<AuthState | null>(null);
  const toastId = useRef(1);
  const prevActive = useRef(0);

  const refreshAuth = useCallback(() => {
    api
      .me()
      .then(setAuth)
      // If the endpoint is unreachable (e.g. an older server), assume no auth so
      // the workspace still loads rather than hanging on the spinner.
      .catch(() => setAuth({ required: false, authenticated: false }));
  }, []);

  useEffect(() => {
    refreshAuth();
    const onUnauthorized = () => refreshAuth();
    window.addEventListener("bs-unauthorized", onUnauthorized);
    return () => window.removeEventListener("bs-unauthorized", onUnauthorized);
  }, [refreshAuth]);

  const openCompany = useCallback((edrpou: string) => {
    setCompanyEdrpou(edrpou);
    navigate("company", [edrpou]);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("bs-theme", theme);
  }, [theme]);

  const refreshStatus = useCallback(() => {
    api
      .status()
      .then((s) => {
        setStatus(s);
        setStatusError(null);
      })
      .catch((err) => setStatusError(err?.message ?? "Cannot reach the server"));
  }, []);

  const refreshJobs = useCallback(() => {
    api.jobs().then(setJobs).catch(() => {});
  }, []);

  const dismissToast = useCallback((id: number) => {
    setToasts((list) => list.filter((t) => t.id !== id));
  }, []);

  const toast = useCallback(
    (message: string, kind: ToastKind = "info") => {
      const id = toastId.current++;
      setToasts((list) => [...list, { id, kind, message }]);
      window.setTimeout(() => dismissToast(id), 4500);
    },
    [dismissToast],
  );

  // Initial load + polling. Jobs poll fast; status follows whenever a job
  // finishes (rows/size may have changed).
  useEffect(() => {
    refreshStatus();
    refreshJobs();
    const timer = window.setInterval(refreshJobs, 1500);
    return () => window.clearInterval(timer);
  }, [refreshStatus, refreshJobs]);

  const activeJobs = useMemo(
    () =>
      jobs.filter((j) => j.status === "running" || j.status === "queued").length,
    [jobs],
  );

  useEffect(() => {
    // When the number of active jobs drops, something finished — refresh status.
    if (activeJobs < prevActive.current) {
      refreshStatus();
    }
    prevActive.current = activeJobs;
  }, [activeJobs, refreshStatus]);

  const capabilities = capabilitiesForAuth(auth);
  const value: StoreValue = {
    status,
    statusError,
    refreshStatus,
    jobs,
    activeJobs,
    refreshJobs,
    toasts,
    toast,
    dismissToast,
    theme,
    toggleTheme: () => setTheme((t) => (t === "dark" ? "light" : "dark")),
    companyEdrpou,
    openCompany,
    auth,
    refreshAuth,
    needsLogin: auth ? auth.required && !auth.authenticated : false,
    ...capabilities,
  };

  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

export function useStore(): StoreValue {
  const ctx = useContext(StoreContext);
  if (!ctx) throw new Error("useStore must be used within StoreProvider");
  return ctx;
}
