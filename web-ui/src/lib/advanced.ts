import type {
  ConditionOp,
  ConditionValue,
  FieldDto,
  FieldRef,
  QueryCondition,
  QueryExpr,
} from "../api/types";

export interface ConditionDraft {
  fieldId: string;
  op: ConditionOp;
  single: string;
  from: string;
  to: string;
  negated?: boolean;
}

export interface AdvancedConditionDraft extends ConditionDraft {
  kind: "condition";
  key: number;
  fallbackField?: FieldRef;
}

export interface AdvancedGroupDraft {
  kind: "group";
  key: number;
  op: "And" | "Or";
  negated: boolean;
  children: AdvancedDraftNode[];
  synthetic?: boolean;
}

export type AdvancedDraftNode = AdvancedConditionDraft | AdvancedGroupDraft;

let sequence = 1;

function nextKey(): number {
  return sequence++;
}

export function fieldRefOf(field: FieldDto): FieldRef {
  switch (field.source.kind) {
    case "column":
      return { Column: field.source.name };
    case "extra":
      return { Extra: field.source.header };
    case "source_field":
      return { SourceField: field.source.field_id };
  }
}

function sameField(left: FieldRef, right: FieldRef): boolean {
  if ("Column" in left && "Column" in right) return left.Column === right.Column;
  if ("Extra" in left && "Extra" in right) return left.Extra === right.Extra;
  return false;
}

function fieldIdFor(reference: FieldRef, fields: FieldDto[]): string {
  return fields.find((field) => sameField(fieldRefOf(field), reference))?.id ?? "";
}

export function newAdvancedCondition(fields: FieldDto[]): AdvancedConditionDraft {
  const field = fields[0];
  return {
    kind: "condition",
    key: nextKey(),
    fieldId: field?.id ?? "",
    op: field?.operators[0] ?? "Contains",
    single: "",
    from: "",
    to: "",
    negated: false,
  };
}

export function newAdvancedGroup(fields: FieldDto[]): AdvancedGroupDraft {
  return {
    kind: "group",
    key: nextKey(),
    op: "And",
    negated: false,
    children: [newAdvancedCondition(fields)],
  };
}

export function emptyAdvancedRoot(): AdvancedGroupDraft {
  return {
    kind: "group",
    key: nextKey(),
    op: "And",
    negated: false,
    children: [],
    synthetic: true,
  };
}

export function draftIsEmpty(draft: ConditionDraft): boolean {
  switch (draft.op) {
    case "IsEmpty":
    case "IsNotEmpty":
      return false;
    case "Range":
      return !draft.from.trim() && !draft.to.trim();
    case "IsAnyOf":
      return draft.single.split(",").every((part) => part.trim() === "");
    default:
      return draft.single.trim() === "";
  }
}

function valueOf(draft: ConditionDraft): ConditionValue {
  switch (draft.op) {
    case "IsEmpty":
    case "IsNotEmpty":
      return "None";
    case "Range":
      return {
        Range: {
          from: draft.from.trim() ? draft.from.trim() : null,
          to: draft.to.trim() ? draft.to.trim() : null,
        },
      };
    case "IsAnyOf":
      return {
        List: draft.single
          .split(",")
          .map((part) => part.trim())
          .filter((part) => part !== ""),
      };
    default:
      return { Single: draft.single.trim() };
  }
}

function draftValue(value: ConditionValue): Pick<ConditionDraft, "single" | "from" | "to"> {
  if (value === "None") return { single: "", from: "", to: "" };
  if ("Single" in value) return { single: value.Single, from: "", to: "" };
  if ("List" in value) return { single: value.List.join(", "), from: "", to: "" };
  return {
    single: "",
    from: value.Range.from ?? "",
    to: value.Range.to ?? "",
  };
}

function conditionToDraft(
  condition: QueryCondition,
  fields: FieldDto[],
): AdvancedConditionDraft {
  const fieldId = fieldIdFor(condition.field, fields);
  return {
    kind: "condition",
    key: nextKey(),
    fieldId,
    fallbackField: fieldId ? undefined : condition.field,
    op: condition.op,
    negated: condition.negated,
    ...draftValue(condition.value),
  };
}

function expressionNodeToDraft(expr: QueryExpr, fields: FieldDto[]): AdvancedDraftNode {
  if ("Condition" in expr) return conditionToDraft(expr.Condition, fields);
  return {
    kind: "group",
    key: nextKey(),
    op: expr.Group.op,
    negated: expr.Group.negated,
    children: expr.Group.children.map((child) => expressionNodeToDraft(child, fields)),
  };
}

export function expressionToAdvancedRoot(
  expr: QueryExpr | null | undefined,
  fields: FieldDto[],
): AdvancedGroupDraft {
  if (!expr) return emptyAdvancedRoot();
  const node = expressionNodeToDraft(expr, fields);
  if (node.kind === "group") return node;
  return {
    kind: "group",
    key: nextKey(),
    op: "And",
    negated: false,
    children: [node],
    synthetic: true,
  };
}

function conditionFromDraft(
  draft: AdvancedConditionDraft,
  fields: FieldDto[],
): QueryExpr | null {
  const field = fields.find((candidate) => candidate.id === draft.fieldId);
  const reference = field ? fieldRefOf(field) : draft.fallbackField;
  if (!reference || draftIsEmpty(draft)) return null;
  return {
    Condition: {
      field: reference,
      op: draft.op,
      value: valueOf(draft),
      negated: Boolean(draft.negated),
    },
  };
}

function expressionFromNode(node: AdvancedDraftNode, fields: FieldDto[]): QueryExpr | null {
  if (node.kind === "condition") return conditionFromDraft(node, fields);
  const children = node.children
    .map((child) => expressionFromNode(child, fields))
    .filter((child): child is QueryExpr => child !== null);
  if (children.length === 0) return null;
  if (node.synthetic && !node.negated && node.op === "And" && children.length === 1) {
    return children[0];
  }
  return {
    Group: {
      op: node.op,
      negated: node.negated,
      children,
    },
  };
}

export function advancedRootToExpression(
  root: AdvancedGroupDraft,
  fields: FieldDto[],
): QueryExpr | null {
  return expressionFromNode(root, fields);
}

export function buildAdvanced(
  drafts: ConditionDraft[],
  fields: FieldDto[],
): QueryExpr | null {
  const root = emptyAdvancedRoot();
  root.synthetic = false;
  root.children = drafts.map((draft) => ({
    kind: "condition" as const,
    key: nextKey(),
    ...draft,
    negated: Boolean(draft.negated),
  }));
  return advancedRootToExpression(root, fields);
}

export const OP_LABELS: Record<ConditionOp, string> = {
  Contains: "contains",
  Equals: "equals",
  StartsWith: "starts with",
  IsAnyOf: "is any of",
  Range: "between",
  IsEmpty: "is empty",
  IsNotEmpty: "is not empty",
};
