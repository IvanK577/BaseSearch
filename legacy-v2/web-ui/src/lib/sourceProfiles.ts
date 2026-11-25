import type {
  ColumnPeek,
  FixedSemanticField,
  SemanticField,
  SourceMappingProfile,
} from "../api/types";

export type ColumnOverrides = Record<number, SemanticField | null>;
export type FixedValueOverrides = Partial<Record<FixedSemanticField, string>>;

export function mappingSelection(
  columnIndex: number,
  overrides: ColumnOverrides,
  profile: SourceMappingProfile | null,
): SemanticField | null | undefined {
  if (Object.prototype.hasOwnProperty.call(overrides, columnIndex)) {
    return overrides[columnIndex];
  }
  if (profile && columnIndex < profile.mapping.length) {
    return profile.mapping[columnIndex];
  }
  return undefined;
}

export function effectiveMapping(
  columns: ColumnPeek[],
  overrides: ColumnOverrides,
  profile: SourceMappingProfile | null,
): (SemanticField | null)[] {
  return columns.map((column) => {
    const selection = mappingSelection(column.index, overrides, profile);
    return selection === undefined ? column.semantic : selection;
  });
}

export function effectiveFixedValue(
  semantic: FixedSemanticField,
  overrides: FixedValueOverrides,
  profile: SourceMappingProfile | null,
): string {
  if (Object.prototype.hasOwnProperty.call(overrides, semantic)) {
    return overrides[semantic] ?? "";
  }
  return profile?.fixed_values[semantic] ?? "";
}
