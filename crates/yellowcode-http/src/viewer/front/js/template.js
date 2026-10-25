import { mountDcs } from "./dialogs.js";

import { el, fill, portions } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { registerKind, update, enableFloatingGeometry } from "./tabs.js";

const TABLE_ROWS = 300;

function sheetView(win, d) {
  const facts = [
    [T.pathCol, win.path],
    [T.kindCol, T.viewSheet],
    [T.sheetRows, d.total_rows],
    [T.sheetColumns, (d.columns || []).length],
    [T.sheetMerges, (d.merges || []).length],
  ];
  return el("div", { class: "tplview", "data-view": "Sheet" },
    el("h3", { role: "note" }, T.inDevelopment),
    el("p", { class: "msg" }, T.sheetPending),
    table(facts, [
      { title: T.prop, cell: (f) => f[0] }, { title: T.valueCol, cell: (f) => f[1] },
    ], T.technicalInfo));
}

const text = (v) => (v === null || v === undefined ? "" : String(v));

function table(rows, cols, label) {
  const body = el("tbody");
  const foot = el("div", { class: "fmore" });
  portions(body, foot, rows, (r) => el("tr", null, cols.map((c) => el("td", null, text(c.cell(r))))), TABLE_ROWS, (n) => T.moreRows + n);
  return el("div", { class: "tbl" },
    el("table", { class: "ftable", "aria-label": label },
      el("thead", null, el("tr", null, cols.map((c) => el("th", null, c.title)))), body), foot);
}

function section(title, count, node, open) {
  return el("details", { class: "dsec", open: open ? "" : null },
    el("summary", null, title + " (" + count + ")"), node);
}

function otherView(d) {
  const root = el("div", { class: "tplview", "data-view": "Other", "data-kind": d.kind },
    el("h3", null, d.kind));
  if (d.kind === "SpreadsheetDocument") {
    root.appendChild(el("h3", { role: "note" }, T.inDevelopment));
    root.appendChild(el("p", { class: "msg" }, T.sheetPending));
  }
  if (d.too_large) {
    root.appendChild(el("div", { class: "msg", role: "note" }, T.tooLarge));
    return root;
  }
  const files = d.files || [];
  root.appendChild(section(T.filesH, files.length, files.length ? table(files, [
    { title: T.name, cell: (f) => f[0] }, { title: T.sizeCol, cell: (f) => f[1] },
  ], T.filesH) : el("div", { class: "msg" }, T.none), true));
  if (d.text !== null && d.text !== undefined) root.appendChild(el("pre", { class: "tpltext" }, d.text));
  return root;
}

function createTemplate(win) {
  const host = el("div", { class: "tplwin" }, el("div", { class: "msg" }, T.loading));
  win.pane.appendChild(host);
  let controller;
  api("template", { path: win.path, from: 0, rows: 1 }).then(
    async (d) => {
      if (win.closed) return;
      if (d.view === "Dcs") {
        enableFloatingGeometry(win);
        const parts = win.path.split(".");
        const owner = parts.length > 2 ? (parts[0] === "Report" ? "Отчет " : parts[0] + " ") + parts[1] + ": " : "";
        update(win, { title: owner + parts.at(-1) + (document.documentElement.lang === "en" ? " [Read only]" : " [Только для чтения]") });
        controller = await mountDcs(win, d);
        return;
      }
      const view = d.view === "Sheet" ? sheetView(win, d) : otherView(d);
      fill(host, el("div", { class: "wtitle" }, el("span", { class: "kind" }, d.view === "Sheet" ? T.viewSheet : d.view === "Dcs" ? T.viewDcs : d.kind), el("span", { class: "name" }, win.title)), view);
    },
  ).catch((e) => { if (!win.closed) fill(host, el("div", { class: "msg bad" }, T.error + e.message)); });
  return { destroy() { controller?.destroy(); } };
}

export function initTemplate() {
  registerKind("template", { create: createTemplate });
}
