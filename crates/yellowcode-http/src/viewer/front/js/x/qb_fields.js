import { icon, kindIconKey } from "../icon.js";
import { el, fill, clear, portions } from "../dom.js";
import { addRow, btn, empty, fieldList, flatSources, iconBtn, textEdit, tools, transfers } from "./qb_tabs.js";
import { tx } from "./waiting.js";
import { installFieldSplitters } from "./qb_splitters.js";

const SHOWN_TABLES = 300;
const DRAG_TYPE = "text/plain";

const baseName = (t) => String(t || "").replace(/\(.*$/s, "").trim().toLowerCase();

function aliasOf(table, taken) {
  const last = String(table).replace(/\(.*$/s, "").split(".").pop() || "Т";
  let a = last.replace(/[^\p{L}\p{N}_]/gu, "_");
  if (!a || /^\d/.test(a)) a = "_" + a;
  let n = a;
  for (let i = 2; taken.has(n.toLowerCase()); i++) n = a + i;
  return n;
}

const tableIcon = (table) => icon(kindIconKey(table.split(".")[0]));

export function createFieldsTab(ctx) {
  const root = el("div", { class: "xb-tab xb-fields" });
  const expanded = new Set();
  let selAlias = null;
  let treeSig = null;
  let treeSeq = 0;
  let selectedTable = null;
  let selectedField = null;

  const filter = el("input", { class: "xq-pin xb-in xb-filter", type: "search", placeholder: tx("Фильтр таблиц", "Filter tables"), "aria-label": tx("Фильтр таблиц", "Filter tables"), autocomplete: "off" });
  const tree = el("div", { class: "xb-tree", role: "tree" });
  filter.addEventListener("input", () => renderTree(true));

  function addTable(name) {
    const v = ctx.view();
    const taken = new Set(flatSources(v && v.tabs).map((s) => s.alias.toLowerCase()));
    return ctx.apply({ op: "add_table", q: ctx.q(), op_index: ctx.opi(), table: name, alias: aliasOf(name, taken) });
  }

  async function addFieldOf(table, field) {
    const v = ctx.view();
    let src = flatSources(v && v.tabs).find((s) => baseName(s.table) === baseName(table));
    let alias = src ? src.alias : null;
    if (!alias) {
      const taken = new Set(flatSources(v && v.tabs).map((s) => s.alias.toLowerCase()));
      alias = aliasOf(table, taken);
      if (!(await ctx.apply({ op: "add_table", q: ctx.q(), op_index: ctx.opi(), table, alias }))) return false;
    }
    return ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr: alias + "." + field });
  }

  function dragSource(node, payload) {
    node.draggable = true;
    node.addEventListener("dragstart", (e) => {
      if (e.dataTransfer) {
        e.dataTransfer.setData(DRAG_TYPE, JSON.stringify(payload));
        e.dataTransfer.effectAllowed = "copy";
      }
    });
  }

  function dropTarget(node) {
    node.addEventListener("dragover", (e) => {
      e.preventDefault();
      node.classList.add("drop");
    });
    node.addEventListener("dragleave", () => node.classList.remove("drop"));
    node.addEventListener("drop", (e) => {
      e.preventDefault();
      node.classList.remove("drop");
      let p = null;
      try {
        p = JSON.parse(e.dataTransfer ? e.dataTransfer.getData(DRAG_TYPE) : "null");
      } catch (err) {
        p = null;
      }
      if (!p || typeof p.t !== "string") return;
      if (typeof p.f === "string") addFieldOf(p.t, p.f);
      else addTable(p.t);
    });
  }

  function tableNode(stub, depth) {
    const key = stub.name;
    const open = expanded.has(key);
    const body = el("div", { class: "xb-tbody", role: "group" });
    const has = stub.fields > 0 || stub.children > 0;
    const tw = el("span", { class: "tw" }, has ? (open ? "▾" : "▸") : "");
    const head = el("div", { class: "xb-trow xb-tbl", role: "treeitem", "data-table": stub.name, title: stub.name, style: "padding-left:" + (6 + depth * 14) + "px" },
      tw, tableIcon(stub.name), el("span", { class: "xb-tname" }, stub.name.split(".").pop()));
    head.addEventListener("click", () => {
      selectedTable = stub.name;
      tree.querySelectorAll(".xb-tbl").forEach((node) => node.classList.toggle("cur", node === head));
    });
    head.addEventListener("dblclick", () => addTable(stub.name));
    tw.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!has) return;
      if (expanded.has(key)) expanded.delete(key);
      else expanded.add(key);
      tw.textContent = expanded.has(key) ? "▾" : "▸";
      fillBody();
    });
    dragSource(head, { t: stub.name });
    function fillBody() {
      clear(body);
      if (!expanded.has(key)) return;
      body.appendChild(el("div", { class: "msg xb-load", style: "padding-left:" + (20 + depth * 14) + "px" }, tx("Загрузка…", "Loading…")));
      const mine = treeSeq;
      ctx.table(stub.name).then((t) => {
        if (mine !== treeSeq || !body.isConnected) return;
        clear(body);
        if (!t) return body.appendChild(el("div", { class: "dim", style: "padding-left:" + (20 + depth * 14) + "px" }, tx("Нет данных о таблице", "No data about the table")));
        if (t.params && t.params.length) {
          body.appendChild(el("div", { class: "dim xb-tparams", style: "padding-left:" + (20 + depth * 14) + "px" }, tx("параметры: ", "parameters: ") + t.params.map((p) => p.name).join(", ")));
        }
        for (const f of t.fields || []) {
          const row = el("div", { class: "xb-trow xb-fld", role: "treeitem", "data-field": f.name, title: f.ty, style: "padding-left:" + (20 + depth * 14) + "px" },
            el("span", { class: "xb-tname" }, f.name), el("span", { class: "xb-fty dim" }, f.ty));
          row.addEventListener("dblclick", () => addFieldOf(stub.name, f.name));
          dragSource(row, { t: stub.name, f: f.name });
          body.appendChild(row);
        }
        for (const c of t.children || []) body.appendChild(tableNode(c, depth + 1));
      });
    }
    const node = el("div", { class: "xb-tnode" }, head, body);
    if (open) fillBody();
    return node;
  }

  function groupNode(title, tables, key, filtering) {
    const open = filtering || expanded.has("g:" + key);
    const tw = el("span", { class: "tw" }, open ? "▾" : "▸");
    const labels = { Catalogs: "Справочники", Documents: "Документы", Constants: "Константы", Enums: "Перечисления", InformationRegisters: "Регистры сведений", AccumulationRegisters: "Регистры накопления", AccountingRegisters: "Регистры бухгалтерии", CalculationRegisters: "Регистры расчета", ExchangePlans: "Планы обмена", ChartsOfCharacteristicTypes: "Планы видов характеристик", ChartsOfAccounts: "Планы счетов", ChartsOfCalculationTypes: "Планы видов расчета" };
    const head = el("div", { class: "xb-trow xb-group-head", role: "treeitem", "data-group": key }, tw, tableIcon(tables[0]?.name || "Catalog"), el("span", { class: "xb-tname" }, tx(labels[title] || title, title)), el("span", { class: "dim xb-gcount" }, String(tables.length)));
    const body = el("div", { class: "xb-gbody", role: "group", hidden: !open });
    const foot = el("div", { class: "xb-gfoot" });
    let built = false;
    const build = () => {
      if (built) return;
      built = true;
      portions(body, foot, tables, (t) => tableNode(t, 1), SHOWN_TABLES, (n) => tx("Показать ещё (", "Show more (") + n + ")");
    };
    head.addEventListener("click", () => {
      if (filtering) return;
      const k = "g:" + key;
      if (expanded.has(k)) expanded.delete(k);
      else expanded.add(k);
      body.hidden = !expanded.has(k);
      tw.textContent = body.hidden ? "▸" : "▾";
      if (!body.hidden) build();
    });
    if (open) build();
    return el("div", { class: "xb-gnode" }, head, body, foot);
  }

  function renderTree(force) {
    const a = ctx.avail();
    if (!a) return;
    const q = filter.value.trim().toLowerCase();
    const sig = [q, (a.temp || []).map((t) => t.name).join("|"), (a.groups || []).length, (a.groups || []).reduce((n, g) => n + g.tables.length, 0)].join("#");
    if (!force && sig === treeSig) return;
    treeSig = sig;
    treeSeq++;
    const keep = (t) => !q || t.name.toLowerCase().includes(q);
    const nodes = [];
    const temp = (a.temp || []).map((t) => ({ name: t.name, fields: (t.fields || []).length, children: (t.children || []).length })).filter(keep);
    if (temp.length) nodes.push(groupNode(tx("Временные таблицы", "Temporary tables"), temp, "temp", true));
    for (const g of a.groups || []) {
      const tables = g.tables.filter(keep);
      if (tables.length) nodes.push(groupNode(g.presentation, tables, g.presentation, !!q));
    }
    fill(tree, nodes.length ? nodes : empty(tx("Нет подходящих таблиц", "No matching tables")));
  }

  const srcList = el("div", { class: "xb-srclist" });
  const srcFields = el("div", { class: "xb-srcfields" });
  const midCol = el("div", { class: "xb-col xb-mid" },
    el("div", { class: "xb-head" }, tx("Таблицы", "Tables")), srcList,
    el("div", { class: "xb-source-fields" }, srcFields));
  dropTarget(midCol);

  function renderSources(tabs) {
    const sources = flatSources(tabs);
    if (!sources.some((s) => s.alias === selAlias)) selAlias = sources.length ? sources[0].alias : null;
    const rows = sources.map((s) => {
      const params = (s.params || []).map((p, i) =>
        textEdit(p, (v) => ctx.apply({ op: "set_table_param", q: ctx.q(), op_index: ctx.opi(), alias: s.alias, index: i, expr: v.trim() ? v : null }), { class: "xq-pin xb-in xb-param", "aria-label": tx("Параметр виртуальной таблицы ", "Virtual table parameter ") + (i + 1), placeholder: tx("параметр ", "parameter ") + (i + 1) }));
      return el("div", { class: "xb-src" + (s.alias === selAlias ? " cur" : ""), "data-alias": s.alias },
        el("div", { class: "xb-srcmain" },
          el("span", { class: "xb-srctable", title: s.table }, s.kind === "subquery" ? tx("(подзапрос)", "(subquery)") : s.table),
          textEdit(s.alias, (v) => (v.trim() ? ctx.apply({ op: "set_source_alias", q: ctx.q(), op_index: ctx.opi(), alias: s.alias, to: v.trim() }) : Promise.resolve(false)), { class: "xq-pin xb-in xb-alias", "aria-label": tx("Псевдоним таблицы", "Table alias") }),
          s.kind === "subquery" ? btn(tx("Войти", "Enter"), () => ctx.enter(s.alias), { class: "btn xb-btn xb-enter" }) : null,
          iconBtn("×", tx("Удалить таблицу", "Remove the table"), () => ctx.apply({ op: "remove_source", q: ctx.q(), op_index: ctx.opi(), alias: s.alias }), { class: "xq-ibtn xb-ibtn xb-src-del" })),
        params.length ? el("div", { class: "xb-params" }, params) : null);
    });
    for (const r of rows) {
      r.addEventListener("click", (e) => {
        if (e.target.closest("input,button")) return;
        selAlias = r.dataset.alias;
        renderSources(tabs);
        renderSourceFields();
      });
    }
    fill(srcList, rows,
      tools(tx("Добавить подзапрос", "Add a subquery"), btn(tx("Добавить подзапрос", "Add a subquery"), () => ctx.apply({ op: "add_subquery", q: ctx.q(), op_index: ctx.opi(), alias: aliasOf("Подзапрос", new Set(sources.map((s) => s.alias.toLowerCase()))) }), { class: "btn xb-btn xb-add-sub" })));
  }

  function renderSourceFields() {
    selectedField = null;
    fieldTransfer.querySelector("button").disabled = true;
    const a = ctx.avail();
    const src = a && selAlias ? (a.fields || []).find((f) => f.alias === selAlias) : null;
    if (!selAlias) return clear(srcFields);
    if (!src) return fill(srcFields, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
    if (!src.found) return fill(srcFields, el("div", { class: "dim" }, tx("Поля таблицы недоступны", "The fields of the table are not available")));
    fill(srcFields, (src.fields || []).map((f) => {
      const row = el("div", { class: "xb-trow xb-sfld", "data-field": f.name, title: f.ty }, el("span", { class: "xb-tname" }, f.name), el("span", { class: "xb-fty dim" }, f.ty));
      row.addEventListener("click", () => {
        selectedField = src.alias + "." + f.name;
        fieldTransfer.querySelector("button").disabled = false;
        srcFields.querySelectorAll(".xb-sfld").forEach((node) => node.classList.toggle("cur", node === row));
      });
      row.addEventListener("dblclick", () => ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr: src.alias + "." + f.name }));
      dragSource(row, { t: src.table, f: f.name });
      return row;
    }));
  }

  const fieldRows = el("div", { class: "xb-fieldrows" });
  const rightCol = el("div", { class: "xb-col xb-selcol" }, el("div", { class: "xb-head" }, tx("Поля", "Fields")), fieldRows);
  dropTarget(rightCol);

  function renderFields(v) {
    const tabs = v.tabs;
    const n = tabs.fields.length;
    const rows = tabs.fields.map((f) =>
      el("div", { class: "xb-row xb-fld-row", "data-index": f.index },
        textEdit(f.expr_text, (t) => ctx.apply({ op: "set_field", q: ctx.q(), op_index: ctx.opi(), column: f.index, expr: t }), { class: "xq-pin xb-in xb-expr", "aria-label": tx("Выражение", "Expression") }),
        textEdit(f.alias || "", (t) => (t.trim() ? ctx.apply({ op: "set_alias", q: ctx.q(), column: f.index, alias: t.trim() }) : Promise.resolve(false)), { class: "xq-pin xb-in xb-falias", "aria-label": tx("Псевдоним", "Alias") }),
        iconBtn("↑", tx("Выше", "Move up"), () => ctx.apply({ op: "move_column", q: ctx.q(), column: f.index, to: f.index - 1 }), { disabled: f.index === 0, class: "xq-ibtn xb-ibtn xb-f-up" }),
        iconBtn("↓", tx("Ниже", "Move down"), () => ctx.apply({ op: "move_column", q: ctx.q(), column: f.index, to: f.index + 1 }), { disabled: f.index >= n - 1, class: "xq-ibtn xb-ibtn xb-f-down" }),
        iconBtn("×", tx("Удалить поле", "Remove the field"), () => ctx.apply({ op: "remove_field", q: ctx.q(), op_index: ctx.opi(), column: f.index }), { class: "xq-ibtn xb-ibtn xb-f-del" })));
    fill(fieldRows,
      fieldList(ctx, "xb-dl-fields"),
      rows,
      tools(tx("Добавить поле", "Add a field"), addRow(tx("Добавить", "Add"), tx("выражение, например Т.Код", "expression, e.g. T.Code"), (t) => ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr: t }), null, "xb-dl-fields")));
  }

  const left = el("div", { class: "xb-col xb-left" }, el("div", { class: "xb-head" }, tx("База данных", "Database")), tools(tx("Фильтр таблиц", "Filter tables"), filter), tree);
  const links = btn("↔", () => root.dispatchEvent(new CustomEvent("xb-links", { bubbles: true })), { class: "xb-links-open xb-ibtn", title: tx("Связи таблиц", "Table links"), "aria-label": tx("Связи таблиц", "Table links") });
  midCol.prepend(el("div", { class: "xb-column-tools" }, links));
  const tableTransfer = transfers([() => selectedTable && addTable(selectedTable), null,
    () => selAlias && ctx.apply({ op: "remove_source", q: ctx.q(), op_index: ctx.opi(), alias: selAlias }),
    () => flatSources(ctx.view()?.tabs).reduce((chain, src) => chain.then(() => ctx.apply({ op: "remove_source", q: ctx.q(), op_index: ctx.opi(), alias: src.alias })), Promise.resolve())]);
  const fieldTransfer = transfers([() => selectedField && ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr: selectedField }),
    () => (ctx.avail()?.fields || []).filter((src) => src.alias === selAlias).flatMap((src) => src.fields.map((field) => src.alias + "." + field.name)).reduce((chain, expr) => chain.then(() => ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr })), Promise.resolve()), null,
    () => [...(ctx.view()?.tabs.fields || [])].reverse().reduce((chain, field) => chain.then(() => ctx.apply({ op: "remove_field", q: ctx.q(), op_index: ctx.opi(), column: field.index })), Promise.resolve())]);
  fill(root, left, tableTransfer, midCol, fieldTransfer, rightCol);
  installFieldSplitters(root, tableTransfer, fieldTransfer, midCol, srcList);

  function render() {
    const v = ctx.view();
    if (!v || v.kind !== "select") {
      fill(srcList, empty(v ? tx("Это уничтожение таблицы", "This statement drops a table") : tx("Пакет запросов пуст", "The query batch is empty")));
      selectedField = null;
      fieldTransfer.querySelector("button").disabled = true;
      clear(srcFields);
      clear(fieldRows);
      renderTree(false);
      return;
    }
    renderTree(false);
    renderSources(v.tabs);
    renderSourceFields();
    renderFields(v);
  }
  return { root, render, destroy() { treeSeq++; } };
}
