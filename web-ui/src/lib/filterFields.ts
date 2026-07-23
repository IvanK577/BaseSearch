// The direct customs filter fields shared by Search and Analytics so both
// pages offer exactly the same quick filters.

import type { Filters } from "../api/types";
import type { MessageKey } from "./i18n";

export const FILTER_FIELDS: { key: keyof Filters; labelKey: MessageKey }[] = [
  { key: "year", labelKey: "filter_year" },
  { key: "product_code", labelKey: "filter_product_code" },
  { key: "edrpou", labelKey: "filter_company_code" },
  { key: "recipient", labelKey: "filter_recipient" },
  { key: "sender", labelKey: "filter_sender" },
  { key: "trademark", labelKey: "filter_trademark" },
  { key: "description", labelKey: "filter_description" },
  { key: "origin_country", labelKey: "filter_origin_country" },
  { key: "dispatch_country", labelKey: "filter_dispatch_country" },
  { key: "trade_country", labelKey: "filter_trade_country" },
];
