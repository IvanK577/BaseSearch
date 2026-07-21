import type { ColumnRole, SemanticField } from "../api/types";
import type { MessageKey, Translate } from "./i18n";

export const SEMANTICS: { value: SemanticField; key: MessageKey }[] = [
  { value: "Date", key: "semantic_date" },
  { value: "DeclarationNumber", key: "semantic_document_number" },
  { value: "CompanyCode", key: "semantic_company_code" },
  { value: "Sender", key: "semantic_sender" },
  { value: "Recipient", key: "semantic_recipient" },
  { value: "ProductCode", key: "semantic_product_code" },
  { value: "Description", key: "semantic_description" },
  { value: "Trademark", key: "semantic_trademark" },
  { value: "Country", key: "semantic_country" },
  { value: "OriginCountry", key: "semantic_origin_country" },
  { value: "DispatchCountry", key: "semantic_dispatch_country" },
  { value: "TradeCountry", key: "semantic_trade_country" },
  { value: "Quantity", key: "semantic_quantity" },
  { value: "NetWeight", key: "semantic_net_weight" },
  { value: "GrossWeight", key: "semantic_gross_weight" },
  { value: "Value", key: "semantic_value" },
  { value: "Currency", key: "semantic_currency" },
  { value: "WeightUnit", key: "semantic_weight_unit" },
];

const COLUMN_ROLE_KEYS: Record<ColumnRole, MessageKey> = {
  Text: "column_role_text",
  Number: "column_role_number",
  Date: "semantic_date",
  Year: "common_year",
  Country: "semantic_country",
  Code: "column_role_code",
  Identifier: "column_role_identifier",
  Money: "semantic_value",
  Weight: "semantic_net_weight",
};

const LAYOUT_KEYS: Partial<Record<string, MessageKey>> = {
  "recognized profile": "layout_recognized",
  "registry profile": "layout_registry",
  "wide profile": "layout_wide",
  "semantic profile": "layout_semantic",
  "generic table": "layout_generic",
  "custom mapping": "layout_custom",
  "multi-sheet workbook": "layout_multi_sheet",
};

export function semanticLabel(t: Translate, semantic: SemanticField): string {
  const item = SEMANTICS.find(({ value }) => value === semantic);
  return item ? t(item.key) : semantic;
}

export function columnRoleLabel(t: Translate, role: ColumnRole): string {
  return t(COLUMN_ROLE_KEYS[role]);
}

export function layoutLabel(t: Translate, layout: string): string {
  const key = LAYOUT_KEYS[layout.trim().toLowerCase()];
  return key ? t(key) : layout;
}
