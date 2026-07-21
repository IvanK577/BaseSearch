// The working query shared across Search, Analytics and Exports, so a query
// built on one page carries over to the others.

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import {
  emptyQuery,
  type Filters,
  type Query,
  type QueryExpr,
  type RecordScope,
} from "../api/types";
import { decodeQuery, encodeQuery } from "../lib/queryUrl";
import { parseRouteHash, updateRouteQuery } from "../lib/router";

interface QueryStore {
  // `query` is the last query the user explicitly applied. Analytics, export,
  // reports and risk views must always use this stable value.
  query: Query;
  // Search controls edit a draft until Search is pressed.
  draftQuery: Query;
  setQuery: (q: Query) => void;
  commit: (q?: Query) => void;
  setText: (text: string) => void;
  setFilter: (key: keyof Filters, value: string) => void;
  setAdvanced: (advanced: QueryExpr | null) => void;
  setRecordScope: (scope: RecordScope) => void;
  applyText: (text: string) => void;
  applyFilter: (key: keyof Filters, value: string) => void;
  applyAdvanced: (advanced: QueryExpr | null) => void;
  applyDrilldown: (condition: QueryExpr) => void;
  undo: () => void;
  canUndo: boolean;
  reset: () => void;
  isEmpty: boolean;
  isDirty: boolean;
}

const QueryContext = createContext<QueryStore | null>(null);
const STORED_QUERY_KEY = "base-search.applied-query.v1";

function queryIsEmpty(q: Query): boolean {
  if (q.text.trim()) return false;
  if (q.advanced) return false;
  return Object.values(q.filters).every((v) => !v.trim());
}

function readInitialQuery(): Query {
  if (typeof window === "undefined") return emptyQuery();
  const fromUrl = decodeQuery(parseRouteHash(window.location.hash).query.get("q"));
  if (fromUrl) return fromUrl;
  try {
    return decodeQuery(window.localStorage.getItem(STORED_QUERY_KEY)) ?? emptyQuery();
  } catch {
    return emptyQuery();
  }
}

function persistAppliedQuery(query: Query): void {
  if (typeof window === "undefined") return;
  const encoded = encodeQuery(query);
  try {
    if (queryIsEmpty(query)) window.localStorage.removeItem(STORED_QUERY_KEY);
    else window.localStorage.setItem(STORED_QUERY_KEY, encoded);
  } catch {
    // Storage can be unavailable in hardened/private browser contexts; the URL
    // remains the authoritative shareable state.
  }
  updateRouteQuery("q", queryIsEmpty(query) ? null : encoded);
}

export function QueryProvider({ children }: { children: ReactNode }) {
  const [draftQuery, setDraftQuery] = useState<Query>(readInitialQuery);
  const [query, setAppliedQuery] = useState<Query>(readInitialQuery);
  const [history, setHistory] = useState<Query[]>([]);

  const remember = useCallback((current: Query) => {
    setHistory((items) => {
      const last = items.at(-1);
      if (last && JSON.stringify(last) === JSON.stringify(current)) return items;
      return [...items, current].slice(-20);
    });
  }, []);

  const setQuery = useCallback((next: Query) => {
    setDraftQuery(next);
    setAppliedQuery(next);
    persistAppliedQuery(next);
  }, []);

  const commit = useCallback(
    (next?: Query) => {
      const applied = next ?? draftQuery;
      if (JSON.stringify(applied) !== JSON.stringify(query)) remember(query);
      setAppliedQuery(applied);
      persistAppliedQuery(applied);
    },
    [draftQuery, query, remember],
  );

  const setText = useCallback((text: string) => {
    setDraftQuery((q) => ({ ...q, text }));
  }, []);

  const setFilter = useCallback((key: keyof Filters, value: string) => {
    setDraftQuery((q) => ({ ...q, filters: { ...q.filters, [key]: value } }));
  }, []);

  const setAdvanced = useCallback((advanced: QueryExpr | null) => {
    setDraftQuery((q) => ({ ...q, advanced: advanced ?? undefined }));
  }, []);

  const setRecordScope = useCallback((record_scope: RecordScope) => {
    setDraftQuery((q) => ({ ...q, record_scope }));
  }, []);

  const applyText = useCallback(
    (text: string) => {
      const next = { ...query, text };
      remember(query);
      setDraftQuery(next);
      setAppliedQuery(next);
      persistAppliedQuery(next);
    },
    [query, remember],
  );

  const applyFilter = useCallback(
    (key: keyof Filters, value: string) => {
      const next = { ...query, filters: { ...query.filters, [key]: value } };
      remember(query);
      setDraftQuery(next);
      setAppliedQuery(next);
      persistAppliedQuery(next);
    },
    [query, remember],
  );

  const applyAdvanced = useCallback(
    (advanced: QueryExpr | null) => {
      const next = { ...query, advanced: advanced ?? undefined };
      remember(query);
      setDraftQuery(next);
      setAppliedQuery(next);
      persistAppliedQuery(next);
    },
    [query, remember],
  );

  const applyDrilldown = useCallback(
    (condition: QueryExpr) => {
      const current = query.advanced;
      const advanced: QueryExpr = !current
        ? condition
        : "Group" in current && current.Group.op === "And" && !current.Group.negated
          ? {
              Group: {
                ...current.Group,
                children: [...current.Group.children, condition],
              },
            }
          : {
              Group: {
                op: "And",
                negated: false,
                children: [current, condition],
              },
            };
      const next = { ...query, advanced };
      remember(query);
      setDraftQuery(next);
      setAppliedQuery(next);
      persistAppliedQuery(next);
    },
    [query, remember],
  );

  const undo = useCallback(() => {
    const previous = history.at(-1);
    if (!previous) return;
    setHistory((items) => items.slice(0, -1));
    setDraftQuery(previous);
    setAppliedQuery(previous);
    persistAppliedQuery(previous);
  }, [history]);

  const reset = useCallback(() => {
    const next = emptyQuery();
    remember(query);
    setDraftQuery(next);
    setAppliedQuery(next);
    persistAppliedQuery(next);
  }, [query, remember]);

  const isDirty = useMemo(
    () => JSON.stringify(draftQuery) !== JSON.stringify(query),
    [draftQuery, query],
  );

  const value = useMemo<QueryStore>(
    () => ({
      query,
      draftQuery,
      setQuery,
      commit,
      setText,
      setFilter,
      setAdvanced,
      setRecordScope,
      applyText,
      applyFilter,
      applyAdvanced,
      applyDrilldown,
      undo,
      canUndo: history.length > 0,
      reset,
      isEmpty: queryIsEmpty(query),
      isDirty,
    }),
    [
      query,
      draftQuery,
      setQuery,
      commit,
      setText,
      setFilter,
      setAdvanced,
      setRecordScope,
      applyText,
      applyFilter,
      applyAdvanced,
      applyDrilldown,
      undo,
      history.length,
      reset,
      isDirty,
    ],
  );

  return <QueryContext.Provider value={value}>{children}</QueryContext.Provider>;
}

export function useQueryStore(): QueryStore {
  const ctx = useContext(QueryContext);
  if (!ctx) throw new Error("useQueryStore must be used within QueryProvider");
  return ctx;
}
