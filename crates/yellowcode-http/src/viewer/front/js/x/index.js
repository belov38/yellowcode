import { registerKind, openWindow } from "../tabs.js";
import { createQueryTab } from "./query.js";
import { createQbTab, QB_KIND } from "./qb.js";
import { createDataTab, DATA_KIND } from "./data.js";
import { createCardTab, CARD_KIND } from "./data_card.js";
import { tx } from "./waiting.js";

export { dataTreeItems } from "./data.js";

export const QUERY_PATH = "x/query";
export const QUERY_KIND = "x-query";

registerKind(QUERY_KIND, { create: createQueryTab });
registerKind(QB_KIND, { create: createQbTab });
registerKind(DATA_KIND, { create: createDataTab });
registerKind(CARD_KIND, { create: createCardTab });

export function openQuery() {
  openWindow({ kind: QUERY_KIND, path: QUERY_PATH, title: tx("Консоль запросов", "Query console") });
}
