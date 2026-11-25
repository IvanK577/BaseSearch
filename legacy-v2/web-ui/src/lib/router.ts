import { useMemo, useSyncExternalStore } from "react";

export type Route =
  | "search"
  | "analytics"
  | "risk"
  | "company"
  | "imports"
  | "exports"
  | "columns"
  | "jobs"
  | "settings";

export interface RouteLocation {
  route: Route;
  segments: string[];
  query: URLSearchParams;
}

const ROUTES = new Set<Route>([
  "search",
  "analytics",
  "risk",
  "company",
  "imports",
  "exports",
  "columns",
  "jobs",
  "settings",
]);

const DEFAULT_ROUTE: Route = "search";
const SERVER_HASH = "#/search";

function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

export function parseRouteHash(hash: string): RouteLocation {
  const raw = hash.replace(/^#\/?/, "");
  const [path, query = ""] = raw.split("?", 2);
  const parts = path.split("/").filter(Boolean);
  const candidate = parts[0] as Route | undefined;
  return {
    route: candidate && ROUTES.has(candidate) ? candidate : DEFAULT_ROUTE,
    segments: parts.slice(1).map(decodeSegment),
    query: new URLSearchParams(query),
  };
}

function currentHash(): string {
  return window.location.hash || SERVER_HASH;
}

function subscribe(listener: () => void): () => void {
  window.addEventListener("hashchange", listener);
  return () => window.removeEventListener("hashchange", listener);
}

export function useRouteLocation(): RouteLocation {
  const hash = useSyncExternalStore(subscribe, currentHash, () => SERVER_HASH);
  return useMemo(() => parseRouteHash(hash), [hash]);
}

export function useRoute(): Route {
  return useRouteLocation().route;
}

export function useRouteSegment(index: number): string | null {
  return useRouteLocation().segments[index] ?? null;
}

export function useRouteQuery(name: string): string | null {
  return useRouteLocation().query.get(name);
}

export function navigate(
  route: Route,
  segments: Array<string | number> = [],
  query?: URLSearchParams,
): void {
  const effectiveQuery = query ?? new URLSearchParams();
  if (query === undefined) {
    const appliedQuery = parseRouteHash(currentHash()).query.get("q");
    if (appliedQuery) effectiveQuery.set("q", appliedQuery);
  }
  const suffix = segments.map((segment) => encodeURIComponent(String(segment))).join("/");
  const search = effectiveQuery.toString();
  window.location.hash = `#/${route}${suffix ? `/${suffix}` : ""}${search ? `?${search}` : ""}`;
}

export function updateRouteQuery(name: string, value: string | null): void {
  const location = parseRouteHash(currentHash());
  if (value === null || value === "") location.query.delete(name);
  else location.query.set(name, value);
  navigate(location.route, location.segments, location.query);
}
