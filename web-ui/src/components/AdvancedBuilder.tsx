import { useEffect, useRef, useState } from "react";

import type { ConditionOp, FieldDto, QueryExpr } from "../api/types";
import {
  advancedRootToExpression,
  expressionToAdvancedRoot,
  newAdvancedCondition,
  newAdvancedGroup,
  OP_LABELS,
  type AdvancedConditionDraft,
  type AdvancedDraftNode,
  type AdvancedGroupDraft,
} from "../lib/advanced";
import { Icon } from "./Icon";

interface AdvancedBuilderProps {
  fields: FieldDto[];
  value?: QueryExpr | null;
  onChange: (expr: QueryExpr | null) => void;
}

export function AdvancedBuilder({ fields, value, onChange }: AdvancedBuilderProps) {
  const fingerprint = JSON.stringify(value ?? null);
  const fieldsFingerprint = fields.map((field) => field.id).join("\u0000");
  const [root, setRoot] = useState<AdvancedGroupDraft>(() =>
    expressionToAdvancedRoot(value, fields),
  );
  const lastEmitted = useRef(fingerprint);

  useEffect(() => {
    if (fingerprint !== lastEmitted.current) {
      setRoot(expressionToAdvancedRoot(value, fields));
      lastEmitted.current = fingerprint;
    }
  }, [fieldsFingerprint, fingerprint, fields, value]);

  const commit = (next: AdvancedGroupDraft) => {
    setRoot(next);
    const expression = advancedRootToExpression(next, fields);
    lastEmitted.current = JSON.stringify(expression);
    onChange(expression);
  };

  return (
    <GroupEditor
      group={root}
      fields={fields}
      root
      onChange={commit}
    />
  );
}

function GroupEditor({
  group,
  fields,
  root = false,
  onChange,
  onRemove,
}: {
  group: AdvancedGroupDraft;
  fields: FieldDto[];
  root?: boolean;
  onChange: (group: AdvancedGroupDraft) => void;
  onRemove?: () => void;
}) {
  const replaceChild = (key: number, next: AdvancedDraftNode) => {
    onChange({
      ...group,
      children: group.children.map((child) => (child.key === key ? next : child)),
    });
  };
  const removeChild = (key: number) => {
    onChange({
      ...group,
      children: group.children.filter((child) => child.key !== key),
    });
  };

  return (
    <div
      className={root ? "stack" : "stack advanced-group"}
      style={{ gap: 10 }}
      aria-label={root ? "Advanced query" : "Condition group"}
    >
      <div className="row wrap" style={{ gap: 8, alignItems: "center" }}>
        <div className="segmented" aria-label="Match logic">
          {(["And", "Or"] as const).map((op) => (
            <button
              key={op}
              type="button"
              className={group.op === op ? "active" : ""}
              aria-label={op.toUpperCase()}
              aria-pressed={group.op === op}
              onClick={() => onChange({ ...group, op })}
            >
              {op.toUpperCase()}
            </button>
          ))}
        </div>
        <span className="faint" style={{ fontSize: 12 }}>
          {group.op === "And" ? "Match every item" : "Match any item"}
        </span>
        <div className="grow" />
        <label className="check-row" style={{ margin: 0 }}>
          <input
            type="checkbox"
            checked={group.negated}
            aria-label="Exclude group"
            onChange={(event) => onChange({ ...group, negated: event.target.checked })}
          />
          <span>Exclude group</span>
        </label>
        {!root && onRemove ? (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={onRemove}
            aria-label="Remove group"
          >
            <Icon name="close" size={15} />
          </button>
        ) : null}
      </div>

      {group.children.map((child, index) =>
        child.kind === "condition" ? (
          <ConditionEditor
            key={child.key}
            condition={child}
            index={index}
            fields={fields}
            onChange={(next) => replaceChild(child.key, next)}
            onRemove={() => removeChild(child.key)}
          />
        ) : (
          <GroupEditor
            key={child.key}
            group={child}
            fields={fields}
            onChange={(next) => replaceChild(child.key, next)}
            onRemove={() => removeChild(child.key)}
          />
        ),
      )}

      {group.children.length === 0 ? (
        <div className="faint" style={{ fontSize: 12 }}>
          No advanced conditions yet.
        </div>
      ) : null}

      <div className="row wrap" style={{ gap: 8 }}>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() =>
            onChange({
              ...group,
              children: [...group.children, newAdvancedCondition(fields)],
            })
          }
          disabled={fields.length === 0}
        >
          <Icon name="plus" size={15} /> Add condition
        </button>
        <button
          type="button"
          className="btn btn-sm btn-ghost"
          onClick={() =>
            onChange({
              ...group,
              children: [...group.children, newAdvancedGroup(fields)],
            })
          }
          disabled={fields.length === 0}
        >
          <Icon name="plus" size={15} /> Add group
        </button>
      </div>
    </div>
  );
}

function ConditionEditor({
  condition,
  index,
  fields,
  onChange,
  onRemove,
}: {
  condition: AdvancedConditionDraft;
  index: number;
  fields: FieldDto[];
  onChange: (condition: AdvancedConditionDraft) => void;
  onRemove: () => void;
}) {
  const field = fields.find((candidate) => candidate.id === condition.fieldId);
  const operators = field?.operators ?? [condition.op];
  const unknownLabel = condition.fallbackField
    ? "Column" in condition.fallbackField
      ? condition.fallbackField.Column
      : "Extra" in condition.fallbackField
        ? condition.fallbackField.Extra
        : condition.fallbackField.SourceField
    : null;

  const update = (patch: Partial<AdvancedConditionDraft>) =>
    onChange({ ...condition, ...patch });

  return (
    <div className="row wrap advanced-condition" style={{ gap: 8 }}>
      <select
        className="select"
        style={{ width: 210 }}
        value={condition.fieldId}
        aria-label={`Field ${index + 1}`}
        onChange={(event) => {
          const nextField = fields.find((candidate) => candidate.id === event.target.value);
          update({
            fieldId: event.target.value,
            fallbackField: undefined,
            op: nextField?.operators[0] ?? "Contains",
          });
        }}
      >
        {!field && unknownLabel ? (
          <option value="">Unavailable field: {unknownLabel}</option>
        ) : null}
        {fields.map((candidate) => (
          <option key={candidate.id} value={candidate.id}>
            {candidate.label}
          </option>
        ))}
      </select>

      <select
        className="select"
        style={{ width: 145 }}
        value={condition.op}
        aria-label={`Operator ${index + 1}`}
        onChange={(event) => update({ op: event.target.value as ConditionOp })}
      >
        {operators.map((operator) => (
          <option key={operator} value={operator}>
            {OP_LABELS[operator]}
          </option>
        ))}
      </select>

      {condition.op === "Range" ? (
        <>
          <input
            className="input"
            style={{ width: 120 }}
            placeholder="from"
            aria-label={`From ${index + 1}`}
            value={condition.from}
            onChange={(event) => update({ from: event.target.value })}
          />
          <input
            className="input"
            style={{ width: 120 }}
            placeholder="to"
            aria-label={`To ${index + 1}`}
            value={condition.to}
            onChange={(event) => update({ to: event.target.value })}
          />
        </>
      ) : condition.op === "IsEmpty" || condition.op === "IsNotEmpty" ? null : (
        <input
          className="input grow"
          style={{ minWidth: 170 }}
          placeholder={condition.op === "IsAnyOf" ? "value1, value2, …" : "value"}
          aria-label={`Value ${index + 1}`}
          value={condition.single}
          onChange={(event) => update({ single: event.target.value })}
        />
      )}

      <label className="check-row" style={{ margin: 0 }}>
        <input
          type="checkbox"
          checked={Boolean(condition.negated)}
          aria-label={`Exclude condition ${index + 1}`}
          onChange={(event) => update({ negated: event.target.checked })}
        />
        <span>NOT</span>
      </label>

      <button
        type="button"
        className="btn btn-ghost btn-sm"
        onClick={onRemove}
        aria-label={`Remove condition ${index + 1}`}
      >
        <Icon name="close" size={15} />
      </button>
    </div>
  );
}
