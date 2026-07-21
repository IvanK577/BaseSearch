// Saved searches and recent history, persisted locally in the browser. No
// backend — comfortable recall of frequent queries during long sessions.

import { useSyncExternalStore } from "react";

import type { Filters, Query } from "../api/types";

export interface SearchEntry {
  id: string;
  label: string;
  query: Query;
  ts: number;
}

const RECENT_KEY = "bs-recent-searches";
const SAVED_KEY = "bs-saved-searches";
const RECENT_LIMIT = 12;

const FILTER_LABELS: Record<keyof Filters, string> = {
  year: "year",
  product_code: "code",
  trademark: "brand",
  description: "desc",
  sender: "sender",
  recipient: "recipient",
  edrpou: "edrpou",
  trade_country: "trade",
  dispatch_country: "dispatch",
  origin_country: "origin",
};

export function queryLabel(query: Query): string {
  const parts: string[] = [];
  if (query.text.trim()) parts.push(`"${query.text.trim()}"`);
  for (const key of Object.keys(FILTER_LABELS) as (keyof Filters)[]) {
    const value = query.filters[key].trim();
    if (value) parts.push(`${FILTER_LABELS[key]}:${value}`);
  }
  if (query.advanced) parts.push("advanced");
  return parts.length ? parts.join(" · ") : "All rows";
}

function read(key: string): SearchEntry[] {
  try {
    const raw = localStorage.getItem(key);
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function write(key: string, entries: SearchEntry[]) {
  localStorage.setItem(key, JSON.stringify(entries));
  emit();
}

const listeners = new Set<() => void>();
let version = 0;
function emit() {
  version += 1;
  listeners.forEach((fn) => fn());
}
function subscribe(fn: () => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

function sameQuery(a: Query, b: Query): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function isEmptyQuery(q: Query): boolean {
  return (
    !q.text.trim() &&
    !q.advanced &&
    Object.values(q.filters).every((v) => !v.trim())
  );
}

export function recordSearch(query: Query): void {
  if (isEmptyQuery(query)) return;
  const entries = read(RECENT_KEY).filter((e) => !sameQuery(e.query, query));
  entries.unshift({
    id: `r${Date.now()}`,
    label: queryLabel(query),
    query,
    ts: Date.now(),
  });
  write(RECENT_KEY, entries.slice(0, RECENT_LIMIT));
}

export function saveSearch(query: Query, name?: string): void {
  const label = name?.trim() || queryLabel(query);
  const entries = read(SAVED_KEY).filter((e) => !sameQuery(e.query, query));
  entries.unshift({ id: `s${Date.now()}`, label, query, ts: Date.now() });
  write(SAVED_KEY, entries);
}

export function removeSaved(id: string): void {
  write(SAVED_KEY, read(SAVED_KEY).filter((e) => e.id !== id));
}

export function clearRecents(): void {
  write(RECENT_KEY, []);
}

export function isSaved(query: Query): boolean {
  return read(SAVED_KEY).some((e) => sameQuery(e.query, query));
}

export function useSearches(): { recent: SearchEntry[]; saved: SearchEntry[] } {
  useSyncExternalStore(
    subscribe,
    () => version,
    () => version,
  );
  return { recent: read(RECENT_KEY), saved: read(SAVED_KEY) };
}
