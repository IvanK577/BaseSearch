import {
  emptyFilters,
  type ConditionOp,
  type ConditionValue,
  type FieldRef,
  type Filters,
  type Query,
  type QueryExpr,
} from "../api/types";

const MAX_QUERY_STATE_LENGTH = 32_768;
const MAX_QUERY_DEPTH = 12;
const MAX_QUERY_NODES = 256;
const FILTER_KEYS: (keyof Filters)[] = [
  "year",
  "product_code",
  "trademark",
  "description",
  "sender",
  "recipient",
  "edrpou",
  "trade_country",
  "dispatch_country",
  "origin_country",
];
const CONDITION_OPS = new Set<ConditionOp>([
  "Contains",
  "Equals",
  "StartsWith",
  "IsAnyOf",
  "Range",
  "IsEmpty",
  "IsNotEmpty",
]);

export function encodeQuery(query: Query): string {
  return JSON.stringify(query);
}

export function decodeQuery(value: string | null | undefined): Query | null {
  if (!value || value.length > MAX_QUERY_STATE_LENGTH) return null;
  try {
    const parsed: unknown = JSON.parse(value);
    if (!isObject(parsed) || typeof parsed.text !== "string" || !isObject(parsed.filters)) {
      return null;
    }
    const filters = emptyFilters();
    for (const key of FILTER_KEYS) {
      const field = parsed.filters[key];
      if (field !== undefined && typeof field !== "string") return null;
      filters[key] = field ?? "";
    }
    const scope = parsed.record_scope;
    if (scope !== undefined && scope !== "canonical" && scope !== "occurrences") return null;

    let advanced: QueryExpr | undefined;
    if (parsed.advanced !== undefined && parsed.advanced !== null) {
      const budget = { nodes: 0 };
      if (!isQueryExpr(parsed.advanced, 0, budget)) return null;
      advanced = parsed.advanced;
    }
    return {
      text: parsed.text,
      filters,
      ...(advanced ? { advanced } : {}),
      record_scope: scope ?? "canonical",
    };
  } catch {
    return null;
  }
}

function isQueryExpr(value: unknown, depth: number, budget: { nodes: number }): value is QueryExpr {
  if (!isObject(value) || depth > MAX_QUERY_DEPTH || ++budget.nodes > MAX_QUERY_NODES) {
    return false;
  }
  if ("Condition" in value) {
    const condition = value.Condition;
    return (
      isObject(condition) &&
      isFieldRef(condition.field) &&
      typeof condition.op === "string" &&
      CONDITION_OPS.has(condition.op as ConditionOp) &&
      isConditionValue(condition.value) &&
      typeof condition.negated === "boolean"
    );
  }
  if ("Group" in value) {
    const group = value.Group;
    return (
      isObject(group) &&
      (group.op === "And" || group.op === "Or") &&
      typeof group.negated === "boolean" &&
      Array.isArray(group.children) &&
      group.children.every((child) => isQueryExpr(child, depth + 1, budget))
    );
  }
  return false;
}

function isFieldRef(value: unknown): value is FieldRef {
  if (!isObject(value)) return false;
  if (typeof value.Column === "string") return value.Column.length <= 512;
  if (typeof value.Extra === "string") return value.Extra.length <= 512;
  return false;
}

function isConditionValue(value: unknown): value is ConditionValue {
  if (value === "None") return true;
  if (!isObject(value)) return false;
  if (typeof value.Single === "string") return value.Single.length <= 8_192;
  if (Array.isArray(value.List)) {
    return value.List.length <= 256 && value.List.every((item) => typeof item === "string");
  }
  if (isObject(value.Range)) {
    const { from, to } = value.Range;
    return (
      (from === null || typeof from === "string") &&
      (to === null || typeof to === "string")
    );
  }
  return false;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
