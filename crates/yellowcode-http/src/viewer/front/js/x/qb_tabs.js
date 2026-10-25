import { el, fill } from "../dom.js";
import { kf4a2Text, tx, waitLine } from "./waiting.js";

export function btn(label, onclick, attrs) {
  return el("button", Object.assign({ class: "btn xb-btn", type: "button", onclick }, attrs || {}), label);
}

export function iconBtn(sym, title, onclick, attrs) {
  return el("button", Object.assign({ class: "xq-ibtn xb-ibtn", type: "button", title, "aria-label": title, onclick }, attrs || {}), sym);
}

export function textEdit(value, commit, attrs) {
  const input = el("input", Object.assign({ class: "xq-pin xb-in", type: "text", spellcheck: "false", autocomplete: "off", value }, attrs || {}));
  let shown = value;
  input.addEventListener("change", () => {
    const v = input.value;
    if (v === shown) return;
    Promise.resolve(commit(v)).then((ok) => {
      if (ok) shown = v;
      else input.value = shown;
    });
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") input.blur();
    else if (e.key === "Escape") {
      input.value = shown;
      input.blur();
    }
  });
  return input;
}

export function addRow(label, placeholder, onAdd, extra, listId) {
  const input = el("input", { class: "xq-pin xb-in xb-add-in", type: "text", spellcheck: "false", autocomplete: "off", placeholder, "aria-label": placeholder, list: listId || null });
  const go = () => {
    const v = input.value.trim();
    if (!v) return;
    Promise.resolve(onAdd(v)).then((ok) => {
      if (ok) input.value = "";
    });
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") go();
  });
  return el("div", { class: "xb-addrow" }, extra || null, input, btn(label, go, { class: "btn xb-btn xb-add-btn" }));
}

export function select(options, value, onchange, attrs) {
  const s = el("select", Object.assign({ class: "xb-sel" }, attrs || {}), options.map(([v, label]) => el("option", { value: v }, label)));
  s.value = value;
  if (onchange) s.addEventListener("change", () => onchange(s.value));
  return s;
}

export function checkbox(label, on, onchange, attrs) {
  const c = el("input", Object.assign({ type: "checkbox", checked: !!on }, attrs || {}));
  c.addEventListener("change", () => onchange(c.checked));
  return el("label", { class: "xb-check" }, c, " ", label);
}

export const empty = (text) => el("div", { class: "msg xb-empty" }, text);

export function fieldList(ctx, id) {
  const dl = el("datalist", { id });
  const a = ctx.avail();
  const seen = new Set();
  if (a) {
    for (const src of a.fields || []) {
      for (const f of src.fields || []) {
        const n = src.alias + "." + f.name;
        if (seen.size < 3000 && !seen.has(n)) {
          seen.add(n);
          dl.appendChild(el("option", { value: n }));
        }
      }
    }
  }
  return dl;
}

export function flatSources(tabs) {
  const out = [];
  const walk = (s) => {
    out.push(s);
    for (const j of s.joins || []) walk(j.right);
  };
  for (const s of (tabs && tabs.sources) || []) walk(s);
  return out;
}

function guard(ctx, root, build) {
  const opened = new Set([...root.querySelectorAll(".xb-edit-tools[open]")].map((node) => node.dataset.tool));
  const v = ctx.view();
  if (!v) return fill(root, el("div", { class: "msg xb-state-note" }, tx("Пакет запросов пуст", "The query batch is empty")));
  if (v.kind !== "select") return fill(root, el("div", { class: "msg xb-state-note" }, tx("Это уничтожение временной таблицы: правка на вкладке «Пакет запросов»", "This statement drops a temporary table: edit it on the batch tab")));
  const note = v.truncated && v.truncated.length ? el("div", { class: "xq-cut xb-cut" }, tx("Показаны не все строки: ", "Not all rows are shown: ") + v.truncated.join(", ")) : null;
  fill(root, note, build(v.tabs, v));
  root.querySelectorAll(".xb-edit-tools").forEach((node) => { node.open = opened.has(node.dataset.tool); });
}

const JOIN_KINDS = () => [
  ["inner", tx("Внутреннее", "Inner")],
  ["left", tx("Левое", "Left")],
  ["right", tx("Правое", "Right")],
  ["full", tx("Полное", "Full")],
];

export function joinTypeOf(text) {
  const t = String(text || "").toUpperCase();
  if (/ЛЕВ|LEFT/.test(t)) return "left";
  if (/ПРАВ|RIGHT/.test(t)) return "right";
  if (/ПОЛН|FULL/.test(t)) return "full";
  return "inner";
}

export function createLinksTab(ctx) {
  const root = el("div", { class: "xb-tab xb-links" });
  const render = () =>
    guard(ctx, root, (tabs) => {
      const aliases = flatSources(tabs).map((s) => [s.alias, s.alias]);
      const rows = (tabs.links || []).map((l, i) =>
        el("tr", { class: "xb-link", "data-right": l.right },
          el("td", null, l.left),
          el("td", null, "→"),
          el("td", null, l.right),
          el("td", null, select(JOIN_KINDS(), joinTypeOf(l.kind), (v) => ctx.apply({ op: "set_join", q: ctx.q(), op_index: ctx.opi(), right: l.right, kind: v }), { "aria-label": tx("Вид соединения", "Join type") })),
          el("td", { class: "xb-wide" }, textEdit(l.on_text || "", (v) => ctx.apply({ op: "set_join", q: ctx.q(), op_index: ctx.opi(), right: l.right, on: v }), { "aria-label": tx("Условие связи", "Join condition") })),
          el("td", null, iconBtn("×", tx("Удалить связь", "Remove the link"), () => ctx.apply({ op: "remove_join", q: ctx.q(), op_index: ctx.opi(), right: l.right }), { class: "xq-ibtn xb-ibtn xb-link-del", "data-index": i }))));
      const list = rows.length
        ? el("table", { class: "xb-table" }, el("thead", null, el("tr", null, el("th", null, tx("Левая", "Left")), el("th"), el("th", null, tx("Правая", "Right")), el("th", null, tx("Вид", "Type")), el("th", null, tx("Условие", "Condition")), el("th"))), el("tbody", null, rows))
        : empty(tx("Соединений нет", "No joins"));
      const left = select(aliases, aliases.length ? aliases[0][0] : "", null, { class: "xb-sel xb-j-left", "aria-label": tx("Левая таблица", "Left table") });
      const right = select(aliases, aliases.length > 1 ? aliases[1][0] : "", null, { class: "xb-sel xb-j-right", "aria-label": tx("Правая таблица", "Right table") });
      const kind = select(JOIN_KINDS(), "inner", null, { class: "xb-sel xb-j-kind", "aria-label": tx("Вид соединения", "Join type") });
      const form = addRow(tx("Добавить связь", "Add link"), tx("условие, например А.Ссылка = Б.Владелец", "condition, e.g. A.Ref = B.Owner"), (on) =>
        ctx.apply({ op: "add_join", q: ctx.q(), op_index: ctx.opi(), left: left.value, right: right.value, kind: kind.value, on }),
      el("span", { class: "xb-inline" }, left, right, kind));
      return el("div", null, list, form);
    });
  return { root, render };
}

export function createConditionsTab(ctx) {
  const root = el("div", { class: "xb-tab xb-conds" });
  const section = (title, rows, cls, offset) =>
    el("div", { class: "xb-sect" }, el("h4", null, title),
      rows.length
        ? rows.map((r) =>
            el("div", { class: "xb-row " + cls, "data-index": r.index },
              textEdit(r.text, (v) => ctx.apply({ op: "set_condition", q: ctx.q(), op_index: ctx.opi(), index: offset + r.index, expr: v }), { "aria-label": tx("Условие", "Condition") }),
              iconBtn("×", tx("Удалить условие", "Remove the condition"), () => ctx.apply({ op: "remove_condition", q: ctx.q(), op_index: ctx.opi(), index: offset + r.index }), { class: "xq-ibtn xb-ibtn xb-cond-del" })))
        : empty(tx("Нет", "None")));
  const render = () =>
    guard(ctx, root, (tabs) => {
      const c = tabs.conditions || { filter: [], having: [] };
      const picker = fieldsPicker(ctx);
      const form = addRow(tx("Добавить условие", "Add a condition"), tx("условие, например Т.Код > 5", "condition, e.g. T.Code > 5"), (v) => ctx.apply({ op: "add_condition", q: ctx.q(), op_index: ctx.opi(), expr: v }), null, "xb-dl-cond");
      return el("div", { class: "xb-native-conditions" }, picker.root,
        el("div", { class: "xb-native-right" }, fieldList(ctx, "xb-dl-cond"), tools(tx("Добавить условие", "Add a condition"), form),
          sheet([tx("Номер", "Number"), tx("П.", "Use"), tx("Условие", "Condition")],
            el("div", { class: "xb-condition-rows" }, section("", c.filter, "xb-cond", 0), c.having.length ? section(tx("Условия на агрегаты", "Aggregate conditions"), c.having, "xb-having", c.filter.length) : null))));
    });
  return { root, render };
}

const AGGREGATES = [["СУММА", "SUM"], ["КОЛИЧЕСТВО", "COUNT"], ["МИНИМУМ", "MIN"], ["МАКСИМУМ", "MAX"], ["СРЕДНЕЕ", "AVG"]];
const AGG_RE = /^\s*(СУММА|КОЛИЧЕСТВО|МИНИМУМ|МАКСИМУМ|СРЕДНЕЕ|SUM|COUNT|MIN|MAX|AVG|AVERAGE)\s*\(/i;

export function createGroupTab(ctx) {
  const root = el("div", { class: "xb-tab xb-group" });
  const render = () =>
    guard(ctx, root, (tabs) => {
      const g = tabs.group || { kind: null, rows: [] };
      const sets = g.kind === "sets";
      const groupRows = g.rows.length
        ? g.rows.map((r) =>
            el("div", { class: "xb-row xb-grp", "data-index": r.index },
              el("span", { class: "xb-text" }, r.text),
              sets ? null : iconBtn("×", tx("Убрать из группировки", "Remove from the grouping"), () => ctx.apply({ op: "remove_group", q: ctx.q(), op_index: ctx.opi(), index: r.index }), { class: "xq-ibtn xb-ibtn xb-grp-del" })))
        : empty(tx("Группировки нет", "No grouping"));
      const aggs = (tabs.fields || []).filter((f) => AGG_RE.test(f.expr_text));
      const aggRows = aggs.length
        ? aggs.map((f) =>
            el("div", { class: "xb-row xb-agg", "data-index": f.index },
              el("span", { class: "xb-text" }, f.expr_text),
              el("span", { class: "dim" }, f.alias || ""),
              iconBtn("×", tx("Убрать агрегат", "Remove the aggregate"), () => ctx.apply({ op: "remove_field", q: ctx.q(), op_index: ctx.opi(), column: f.index }), { class: "xq-ibtn xb-ibtn xb-agg-del" })))
        : empty(tx("Агрегатов нет", "No aggregates"));
      const fn = select(AGGREGATES.map(([ru, en]) => [ru, ru + " (" + en + ")"]), AGGREGATES[0][0], null, { class: "xb-sel xb-agg-fn", "aria-label": tx("Функция", "Function") });
      const alias = el("input", { class: "xq-pin xb-in xb-agg-alias", type: "text", placeholder: tx("псевдоним", "alias"), "aria-label": tx("Псевдоним агрегата", "Aggregate alias") });
      const picker = fieldsPicker(ctx);
      return el("div", { class: "xb-native-split xb-native-group" }, picker.root,
        transfers([() => picker.value() && ctx.apply({ op: "add_group", q: ctx.q(), op_index: ctx.opi(), expr: picker.value() }), null, null, null]),
        el("div", { class: "xb-native-right" },
          checkbox(tx("Использовать несколько группировок", "Use multiple groupings"), sets, () => {}, { class: "xb-group-sets", disabled: true, title: tx("Наборы группировок правятся в тексте запроса", "Edit grouping sets in the query text") }), fieldList(ctx, "xb-dl-group"),
        el("div", { class: "xb-sect xb-native-sheet" }, el("h4", null, tx("Поле группировки", "Grouping field")),
          sets ? el("div", { class: "xq-wait" }, tx("Наборы группировок правятся только в тексте запроса", "Grouping sets are edited in the query text only")) : null,
          el("div", { class: "xb-sheet-body" }, groupRows),
          sets ? null : tools(tx("Добавить поле группировки", "Add grouping field"), addRow(tx("Добавить", "Add"), tx("поле группировки", "grouping field"), (v) => ctx.apply({ op: "add_group", q: ctx.q(), op_index: ctx.opi(), expr: v }), null, "xb-dl-group"))),
        el("div", { class: "xb-sect xb-native-sheet" }, sheetHeader([tx("Суммируемое поле", "Aggregate field"), tx("Функция", "Function")], "64% minmax(0,1fr)"), el("div", { class: "xb-sheet-body" }, aggRows),
          tools(tx("Добавить агрегат", "Add aggregate"), addRow(tx("Добавить агрегат", "Add aggregate"), tx("поле, например Т.Сумма", "field, e.g. T.Amount"), (v) =>
            ctx.apply({ op: "add_field", q: ctx.q(), op_index: ctx.opi(), expr: fn.value + "(" + v + ")", alias: alias.value.trim() || null }), el("span", { class: "xb-inline" }, fn, alias), "xb-dl-group")))));
    });
  return { root, render };
}

export function createUnionsTab(ctx) {
  const root = el("div", { class: "xb-tab xb-unions" });
  const render = () =>
    guard(ctx, root, (tabs) => {
      const cur = ctx.cur();
      const ops = tabs.unions || [];
      const rows = ops.map((u) =>
        el("tr", { class: "xb-union" + (u.index === cur.op ? " cur" : ""), "data-index": u.index },
          el("td", null, el("button", { class: "btn xb-btn xb-union-pick", type: "button", "aria-pressed": u.index === cur.op ? "true" : "false", onclick: () => ctx.select(cur.stmt, u.index, cur.nested) }, String(u.index + 1))),
          el("td", null, u.index === 0 ? "" : checkbox(tx("ОБЪЕДИНИТЬ ВСЕ", "UNION ALL"), u.union_all, (on) => ctx.apply({ op: "set_union_all", q: ctx.q(), op_index: u.index, all: on }).then((ok) => ok || render()), { class: "xb-union-all" })),
          el("td", { class: "dim" }, tx("полей: ", "fields: ") + u.fields + tx(", таблиц: ", ", tables: ") + u.sources),
          el("td", { class: "xb-text" }, u.title || ""),
          el("td", null, iconBtn("×", tx("Удалить оператор", "Remove the operator"), () => ctx.apply({ op: "remove_operator", q: ctx.q(), op_index: u.index }), { class: "xq-ibtn xb-ibtn xb-union-del" }))));
      const names = (tabs.fields || []).length
        ? (tabs.fields || []).map((f) =>
            el("div", { class: "xb-row xb-name", "data-index": f.index },
              el("span", { class: "xb-text dim" }, f.expr_text),
              textEdit(f.alias || "", (v) => (v.trim() ? ctx.apply({ op: "set_alias", q: ctx.q(), column: f.index, alias: v.trim() }) : Promise.resolve(false)), { "aria-label": tx("Имя колонки", "Column name") })))
        : empty(tx("Полей нет", "No fields"));
      return el("div", { class: "xb-native-unions" },
        el("div", { class: "xb-sect xb-native-sheet" }, sheetHeader([tx("Имя", "Name"), tx("Без дубликатов", "Distinct")], "60% minmax(0,1fr)"),
          el("div", { class: "xb-sheet-body" }, el("table", { class: "xb-table" }, el("tbody", null, rows))),
          tools(tx("Добавить объединение", "Add a union"), btn(tx("Добавить объединение", "Add a union"), () => ctx.apply({ op: "add_operator", q: ctx.q() }), { class: "btn xb-btn xb-union-add" }))),
        el("div", { class: "xb-sect xb-native-sheet" }, sheetHeader([tx("Имя поля", "Field name"), tx("Запрос 1", "Query 1")], "64% minmax(0,1fr)"), el("div", { class: "xb-sheet-body" }, names)));
    });
  return { root, render };
}

const DIRS = () => [
  ["asc", tx("по возрастанию", "ascending")],
  ["desc", tx("по убыванию", "descending")],
  ["hierarchy", tx("иерархия", "hierarchy")],
  ["hierarchy_desc", tx("иерархия, убыв.", "hierarchy, desc.")],
];

export function createOrderTab(ctx) {
  const root = el("div", { class: "xb-tab xb-order" });
  const render = () =>
    guard(ctx, root, (tabs) => {
      const o = tabs.order || { rows: [], autoorder: false };
      const rows = o.rows.length
        ? o.rows.map((r) =>
            el("div", { class: "xb-row xb-ord", "data-index": r.index },
              el("span", { class: "xb-text" }, r.expr_text),
              select(DIRS(), r.dir, (v) => ctx.apply({ op: "set_order_dir", q: ctx.q(), index: r.index, dir: v }).then((ok) => ok || render()), { "aria-label": tx("Направление", "Direction") }),
              iconBtn("×", tx("Убрать из порядка", "Remove from the order"), () => ctx.apply({ op: "remove_order", q: ctx.q(), index: r.index }), { class: "xq-ibtn xb-ibtn xb-ord-del" })))
        : empty(tx("Порядок не задан", "No order"));
      const dir = select(DIRS(), "asc", null, { class: "xb-sel xb-ord-dir", "aria-label": tx("Направление", "Direction") });
      const picker = fieldsPicker(ctx);
      const add = (expr) => ctx.apply({ op: "add_order", q: ctx.q(), expr, dir: "asc" });
      return el("div", { class: "xb-native-split xb-native-order" }, picker.root,
        transfers([() => picker.value() && add(picker.value()), () => picker.values().reduce((chain, f) => chain.then(() => add(f)), Promise.resolve()), null, null]),
        el("div", { class: "xb-native-right" }, fieldList(ctx, "xb-dl-order"),
          sheet([tx("Поле", "Field"), tx("Сортировка", "Sorting")], rows),
          tools(tx("Добавить поле сортировки", "Add an order field"), addRow(tx("Добавить", "Add"), tx("поле или выражение", "field or expression"), (v) => ctx.apply({ op: "add_order", q: ctx.q(), expr: v, dir: dir.value }), el("span", { class: "xb-inline" }, dir), "xb-dl-order")),
          checkbox(tx("Автоупорядочивание", "Autoorder"), o.autoorder, (on) => ctx.apply({ op: "set_autoorder", q: ctx.q(), on }).then((ok) => ok || render()), { class: "xb-autoorder" })));
    });
  return { root, render };
}

const POINT_KINDS = () => [
  ["items", tx("элементы", "items")],
  ["hierarchy", tx("иерархия", "hierarchy")],
  ["hierarchy_only", tx("только иерархия", "hierarchy only")],
];
const OVERALL_RE = /^\s*(ОБЩИЕ|OVERALL)\s*$/i;

export function createTotalsTab(ctx) {
  const root = el("div", { class: "xb-tab xb-totals" });
  const render = () =>
    guard(ctx, root, (tabs) => {
      const t = tabs.totals || { items: [], points: [] };
      const overall = t.points.some((p) => OVERALL_RE.test(p.text));
      const fieldPoints = t.points.filter((p) => !OVERALL_RE.test(p.text));
      const items = t.items.length
        ? t.items.map((r) =>
            el("div", { class: "xb-row xb-total", "data-index": r.index },
              el("span", { class: "xb-text" }, r.expr_text),
              el("span", { class: "dim" }, r.alias || ""),
              iconBtn("×", tx("Убрать итог", "Remove the total"), () => ctx.apply({ op: "remove_total", q: ctx.q(), index: r.index }), { class: "xq-ibtn xb-ibtn xb-total-del" })))
        : empty(tx("Итогов нет", "No totals"));
      const points = fieldPoints.length
        ? fieldPoints.map((p, i) =>
            el("div", { class: "xb-row xb-point", "data-index": i },
              el("span", { class: "xb-text" }, p.text),
              iconBtn("×", tx("Убрать точку", "Remove the point"), () => ctx.apply({ op: "remove_control_point", q: ctx.q(), index: i }), { class: "xq-ibtn xb-ibtn xb-point-del" })))
        : empty(tx("Контрольных точек нет", "No control points"));
      const kind = select(POINT_KINDS(), "items", null, { class: "xb-sel xb-point-kind", "aria-label": tx("Вид точки", "Point kind") });
      const picker = fieldsPicker(ctx);
      return el("div", { class: "xb-native-split xb-native-totals" }, picker.root,
        transfers([() => picker.value() && ctx.apply({ op: "add_control_point", q: ctx.q(), expr: picker.value(), kind: "items" }), null, null, null]),
        el("div", { class: "xb-native-right" }, fieldList(ctx, "xb-dl-totals"),
        el("div", { class: "xb-sect xb-native-sheet xb-total-values" }, sheetHeader([tx("Итоговое поле", "Total field"), tx("Выражение", "Expression")], "64% minmax(0,1fr)"), el("div", { class: "xb-sheet-body" }, items),
          tools(tx("Добавить итог", "Add a total"), addRow(tx("Добавить итог", "Add a total"), tx("выражение, например СУММА(Т.Сумма)", "expression, e.g. SUM(T.Amount)"), (v) => ctx.apply({ op: "add_total", q: ctx.q(), expr: v }), null, "xb-dl-totals"))),
        el("div", { class: "xb-sect xb-native-sheet xb-total-points" }, sheetHeader([tx("Группировочное поле", "Grouping field"), tx("Тип итогов", "Total type"), tx("Псевдоним", "Alias")], "50% 30% minmax(0,1fr)"),
          checkbox(tx("Общие итоги", "Overall totals"), overall, (on) => ctx.apply({ op: "set_overall", q: ctx.q(), on }).then((ok) => ok || render()), { class: "xb-overall" }),
          el("div", { class: "xb-sheet-body" }, points),
          tools(tx("Добавить точку", "Add a point"), addRow(tx("Добавить точку", "Add a point"), tx("поле", "field"), (v) => ctx.apply({ op: "add_control_point", q: ctx.q(), expr: v, kind: kind.value }), el("span", { class: "xb-inline" }, kind), "xb-dl-totals")))));
    });
  return { root, render };
}

export function createBatchTab(ctx) {
  const root = el("div", { class: "xb-tab xb-batch" });
  const render = () => {
    const v = ctx.view();
    const cur = ctx.cur();
    const batch = (v && v.tabs.batch) || [];
    const n = batch.length;
    const rows = batch.map((b) =>
      el("tr", { class: "xb-stmt" + (b.index === cur.stmt ? " cur" : ""), "data-index": b.index },
        el("td", null, el("button", { class: "btn xb-btn xb-stmt-pick", type: "button", "aria-pressed": b.index === cur.stmt ? "true" : "false", onclick: () => ctx.select(b.index, 0, []) }, tx("Запрос пакета ", "Batch query ") + (b.index + 1))),
        el("td", null, b.kind === "drop" ? tx("УНИЧТОЖИТЬ", "DROP") : tx("ВЫБРАТЬ", "SELECT")),
        el("td", { class: "xb-text" }, b.title || ""),
        el("td", { class: "dim" }, b.into ? tx("в ", "into ") + b.into : ""),
        el("td", { class: "xb-acts" },
          iconBtn("↑", tx("Выше", "Move up"), () => ctx.apply({ op: "move_stmt", stmt: b.index, to: b.index - 1 }), { disabled: b.index === 0, class: "xq-ibtn xb-ibtn xb-stmt-up" }),
          iconBtn("↓", tx("Ниже", "Move down"), () => ctx.apply({ op: "move_stmt", stmt: b.index, to: b.index + 1 }), { disabled: b.index >= n - 1, class: "xq-ibtn xb-ibtn xb-stmt-down" }),
          iconBtn("⧉", tx("Копировать запрос", "Copy the query"), () => ctx.apply({ op: "copy_stmt", stmt: b.index }), { class: "xq-ibtn xb-ibtn xb-stmt-copy" }),
          iconBtn("×", tx("Удалить запрос", "Remove the query"), () => ctx.apply({ op: "remove_stmt", stmt: b.index }), { class: "xq-ibtn xb-ibtn xb-stmt-del" }))));
    const place =
      v && v.kind === "select"
        ? el("div", { class: "xb-sect" }, el("h4", null, tx("Поместить результат запроса ", "Put the result of the query ") + (cur.stmt + 1)),
            el("label", { class: "xb-inline" }, tx("во временную таблицу: ", "into the temporary table: "),
              textEdit((v.into && v.into.table) || "", (t) => ctx.apply({ op: "set_placement", q: { stmt: cur.stmt, nested: [] }, table: t.trim() || null }), { class: "xq-pin xb-in xb-into", "aria-label": tx("Временная таблица", "Temporary table") })))
        : v
          ? el("div", { class: "xb-sect" }, el("h4", null, tx("Уничтожаемая таблица", "Table to drop")),
              textEdit(v.drop_table || "", (t) => (t.trim() ? ctx.apply({ op: "set_drop_table", stmt: cur.stmt, table: t.trim() }) : Promise.resolve(false)), { class: "xq-pin xb-in xb-drop-name", "aria-label": tx("Таблица", "Table") }))
          : null;
    fill(root,
      tools(tx("Изменить пакет запросов", "Edit query batch"), el("div", { class: "xb-batch-tools" },
      el("div", { class: "xb-addrow" },
        btn(tx("Добавить запрос", "Add a query"), () => ctx.apply({ op: "add_select" }), { class: "btn xb-btn xb-stmt-add" })),
      addRow(tx("Добавить уничтожение", "Add a drop"), tx("имя временной таблицы", "temporary table name"), (t) => ctx.apply({ op: "add_drop", table: t })),
      place)),
      sheet([tx("Запрос", "Query")], rows.length ? el("table", { class: "xb-table" }, el("tbody", null, rows)) : empty(tx("Пакет пуст", "The batch is empty"))));
  };
  return { root, render };
}

export function createDcsTab(ctx) {
  const root = el("div", { class: "xb-tab xb-dcs" });
  const render = () => {
    const v = ctx.view();
    const dcs = (v && v.tabs.dcs) || { operator: [], query: [] };
    const block = (title, list) =>
      el("div", { class: "xb-sect" }, el("h4", null, title),
        list.length ? list.map((t) => el("pre", { class: "xq-pre xb-dcs-block" }, t)) : empty(tx("Блоков нет", "No blocks")));
    const heads = [tx("Таблицы", "Tables"), tx("Поля", "Fields"), tx("Условия", "Conditions"), tx("Порядок", "Order"), tx("Итоги", "Totals")];
    const panel = sheet([tx("Таблица", "Table"), tx("Обязательная", "Required"), tx("Номер группы", "Group number")],
      [...dcs.operator, ...dcs.query].map((text) => el("pre", { class: "xq-pre xb-dcs-block" }, text)));
    const bar = el("div", { class: "xb-dcs-tabs", role: "tablist" }, heads.map((label, i) => el("button", {
      type: "button", class: "otab" + (i === 0 ? " cur" : ""), role: "tab", "aria-selected": i === 0 ? "true" : "false",
      onclick: (event) => {
        bar.querySelectorAll("button").forEach((button) => { const on = button === event.currentTarget; button.classList.toggle("cur", on); button.setAttribute("aria-selected", String(on)); });
        panel.querySelector(".xb-sheet-head").textContent = label;
      },
    }, label)));
    fill(root, bar, el("div", { class: "xb-dcs-toolbar" }, el("button", { class: "xb-ibtn", type: "button", disabled: true, title: kf4a2Text(), "aria-label": kf4a2Text() }, "⚙")), panel,
      waitLine(tx("Правка блоков компоновки данных — ", "Editing of data composition blocks: ") + kf4a2Text(), "dcs"));
  };
  return { root, render };
}


export function createModifiersTab(ctx) {
  const root = el("div", { class: "xb-tab xb-advanced" });
  const render = () => guard(ctx, root, (tabs, v) => {
    const top = textEdit(v.top == null ? "" : String(v.top), (value) => {
      if (value.trim() && !/^\d+$/.test(value.trim())) return ctx.fail(tx("«Первые»: нужно целое число", "TOP needs a whole number"));
      return ctx.apply({ op: "set_top", q: ctx.q(), op_index: ctx.opi(), n: value.trim() ? Number(value) : null });
    }, { class: "xb-in xb-top", "aria-label": tx("Первые N", "TOP N"), placeholder: "1", inputmode: "numeric" });
    const topEnabled = checkbox(tx("Первые", "First"), v.top != null, (on) => ctx.apply({ op: "set_top", q: ctx.q(), op_index: ctx.opi(), n: on ? 1 : null }).then((ok) => ok || render()));
    const selection = el("fieldset", { class: "xb-selection-options" }, el("legend", null, tx("Выборка записей", "Record selection")),
      el("div", { class: "xb-inline" }, topEnabled, top),
      checkbox(tx("Без повторяющихся", "Distinct"), v.distinct, (on) => ctx.apply({ op: "set_distinct", q: ctx.q(), op_index: ctx.opi(), on }).then((ok) => ok || render()), { class: "xb-distinct" }),
      checkbox(tx("Разрешенные", "Allowed"), v.allowed, (on) => ctx.apply({ op: "set_allowed", q: ctx.q(), on }).then((ok) => ok || render()), { class: "xb-allowed" }));
    const into = v.into?.table || "";
    const placement = textEdit(into, (value) => ctx.apply({ op: "set_placement", q: ctx.q(), table: value.trim() || null }), { class: "xb-in xb-placement", "aria-label": tx("Имя временной таблицы", "Temporary table name") });
    const radioName = "query-type-" + ctx.cur().stmt + "-" + root.closest(".mdi-window")?.dataset.window;
    const queryType = el("fieldset", { class: "xb-query-type" }, el("legend", null, tx("Тип запроса", "Query type")));
    for (const [i, title] of [tx("Выборка данных", "Select data"), tx("Создание временной таблицы", "Create a temporary table"), tx("Добавление во временную таблицу", "Append to a temporary table"), tx("Уничтожение временной таблицы", "Drop a temporary table")].entries()) {
      const radio = el("input", { type: "radio", name: radioName, checked: i === (into ? 1 : 0), disabled: i > 1, title: i > 1 ? tx("Правка через текст запроса", "Edit through query text") : null });
      radio.addEventListener("change", () => {
        if (i === 0) ctx.apply({ op: "set_placement", q: ctx.q(), table: null });
        else { placement.focus(); if (placement.value.trim()) placement.dispatchEvent(new Event("change")); }
      });
      queryType.appendChild(el("label", { class: "xb-check" }, radio, title));
    }
    queryType.appendChild(el("label", { class: "xb-placement-label" }, tx("Имя временной таблицы:", "Temporary table name:"), placement));
    const sources = flatSources(tabs).map((source) => el("div", { class: "xb-trow" }, source.table));
    fill(root, el("div", { class: "xb-advanced-top" }, selection, queryType),
      el("label", { class: "xb-check", title: tx("Правка через текст запроса", "Edit through query text") }, el("input", { type: "checkbox", disabled: true }), tx("Блокировать получаемые данные для последующего изменения", "Lock retrieved data for subsequent changes")),
      el("div", { class: "xb-advanced-tables" }, sheet([tx("Таблицы", "Tables")], sources), transfers([null, null, null, null]), sheet([tx("Таблицы для изменения", "Tables to update")], [])));
    return [...root.childNodes];
  });
  return { root, render };
}

function sheetHeader(headers, columns = "repeat(" + headers.length + ",minmax(0,1fr))") {
  return el("div", { class: "xb-sheet-head", style: "grid-template-columns:" + columns }, headers.map((text) => el("span", null, text)));
}

export function sheet(headers, content, cls = "") {
  return el("div", { class: "xb-sheet " + cls },
    sheetHeader(headers),
    el("div", { class: "xb-sheet-body" }, content));
}

export function tools(label, content) {
  return el("details", { class: "xb-edit-tools", "data-tool": label },
    el("summary", { title: label, "aria-label": label }, el("span", { class: "xb-add-glyph", "aria-hidden": "true" }, "+")),
    el("div", { class: "xb-edit-popover" }, content));
}

export function transfers(actions) {
  return el("div", { class: "xb-transfers" }, [">", ">>", "<", "<<"].map((label, i) =>
    el("button", { class: "btn", type: "button", disabled: !actions[i], "aria-label": [tx("Добавить выбранное", "Add selected"), tx("Добавить все", "Add all"), tx("Удалить выбранное", "Remove selected"), tx("Удалить все", "Remove all")][i], onclick: actions[i] || null }, label)));
}

export function fieldsPicker(ctx) {
  const fields = (ctx.view()?.tabs.fields || []).map((f) => f.expr_text);
  let selected = fields[0] || "";
  const root = sheet([tx("Поля", "Fields")], [
    el("div", { class: "xb-pick-all" }, tx("Все поля", "All fields")),
    ...fields.map((name) => el("button", { class: "xb-pick-field", type: "button", onclick: (event) => {
      selected = name;
      root.querySelectorAll(".xb-pick-field").forEach((node) => node.classList.toggle("cur", node === event.currentTarget));
    } }, name)),
  ], "xb-picker");
  return { root, value: () => selected, values: () => fields };
}
