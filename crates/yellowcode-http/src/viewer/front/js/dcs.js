import { el, fill } from "./dom.js";
import { icon } from "./icon.js";
import { createEditor, replaceText, releaseEditor } from "./editor.js";
import { valueText, browseRows, splitPane } from "./dcs_widgets.js";
import { mountDcsSettings } from "./dcs_settings.js";

export function mountDcsRenderer(win, schema, data, helpers) {
  const { tabs, grid, disable, tr, en } = helpers;
  if (win.closed) return;
  const form = data.forms.dcs;
  const body = el("div", { class: "dialog-body dcs-designer-body service-dialog tplview", "data-view": "Dcs" });
  win.pane.classList.add("dcs-window");
  fill(win.pane, body);
  const ctx = { body };
  const source = new Map();
  const collect = (controls) => controls.forEach((c) => { source.set(c.name, c); collect(c.children); });
  collect(form.controls);
  const pages = tabs(source.get("LayouterBase").tab_pages, ctx.body);
  const button = (label, pic) => {
    const b = el("button", { type: "button", "aria-label": label }, pic ? icon("pic/" + pic) : label);
    disable(b); return b;
  };
  const toolbar = () => el("div", { class: "dcs-toolbar" }, [
    [tr("Добавить", "Add"), "CreateListItem"], [tr("Скопировать", "Copy"), "CloneObject"],
    [tr("Удалить", "Delete"), "Delete"], [tr("Вверх", "Up"), "MoveUp"], [tr("Вниз", "Down"), "MoveDown"],
  ].map(([label, pic]) => button(label, pic)));
  const table = (name, rows = [], columns) => {
    const c = source.get(name);
    const node = grid({ columns: columns || c?.columns || [] });
    node.dataset.dcsControl = name;
    if (name === "GridParams") {
      node.querySelector("table").style.tableLayout = "fixed";
      node.querySelector("table").prepend(el("colgroup", null, [9, 9, 11, 9, 2, 11, 9, 9, 2, 2, 2, 8, 16].map((width) => el("col", { style: `width:${width}%` }))));
      node.querySelectorAll("th").forEach((cell) => { cell.style.minWidth = "0"; cell.title = cell.textContent; });
    }
    const body = node.querySelector("tbody");
    fill(body, rows.slice(0, 300).map((row) => el("tr", null, row.map((value) => el("td", { title: String(value ?? "") }, String(value ?? ""))))));
    if (rows.length && !["GridSettingsVariant", "GridLinks"].includes(name)) body.firstElementChild?.firstElementChild?.classList.add("dcs-selected-cell");
    return node;
  };
  const col = (ru, english, width) => ({ caption: { ru, en: english }, width_serialized: width });
  const check = (ru, english, checked) => {
    const node = el("label", null, el("input", { type: "checkbox", checked }), tr(ru, english));
    disable(node); return node;
  };
  const allSets = [];
  const flatten = (sets, depth = 0) => (sets || []).forEach((set) => { allSets.push({ set, depth }); flatten(set.items, depth + 1); });
  flatten(schema.sets);
  const tree = el("div", { class: "dcs-set-tree", role: "tree", "aria-label": tr("Наборы данных", "Data sets") }, el("div", null, tr("⊟ Наборы данных", "⊟ Data sets")));
  const left = el("section", { class: "dcs-sets" }, toolbar(), tree);
  const fields = table("GridFields", [], [col("Поле", "Field", 120), col("Путь", "Path", 230)]);
  fields.querySelector("table").prepend(el("colgroup", null, [120, 238, 43, 43, 43, 43, 120, 120, 238, 120, 120].map((width) => el("col", { style: `width:${width}px` }))));
  const head = fields.querySelector("thead");
  const th = (text, attrs = {}) => el("th", attrs, text);
  fill(head,
    el("tr", null, th(tr("Поле", "Field"), { rowspan: "4" }), th(tr("Путь", "Path")),
      th(tr("Ограничение поля", "Field restriction"), { colspan: "4" }), th(tr("Роль", "Role"), { rowspan: "4" }),
      th(tr("Выражение представления", "Presentation expression")), th(tr("Проверка иерархии:", "Hierarchy check:")),
      th(tr("Тип значения", "Value type")), th(tr("Оформление", "Appearance"))),
    el("tr", null, th(tr("Заголовок", "Title"), { rowspan: "3" }),
      ...[tr("Поле", "Field"), tr("Условие", "Condition"), tr("Группа", "Group"), tr("Упорядочивание", "Ordering")].map((s) => th(s)),
      th(tr("Выражения упорядочивания", "Ordering expressions"), { rowspan: "3" }), th(tr("Набор данных", "Data set")),
      th(tr("Доступные значения", "Available values"), { rowspan: "3" }), th(tr("Параметры редактирования", "Editing options"), { rowspan: "3" })),
    el("tr", null, th(tr("Ограничение реквизитов", "Attribute restriction"), { colspan: "4" }), th(tr("Параметр", "Parameter"), { rowspan: "2" })),
    el("tr", null, ...[tr("Поле", "Field"), tr("Условие", "Condition"), tr("Группа", "Group"), tr("Упорядочивание", "Ordering")].map((s) => th(s))));
  const query = el("div", { class: "code cm-host dcs-query", "aria-label": tr("Запрос", "Query"), "data-dcs-control": "EditQuery" });
  const queryView = createEditor({ parent: query, readOnly: true, queryLanguage: true });
  const fieldPane = el("section", { class: "dcs-table-page" },
    el("div", { class: "dcs-field-heading" }, tr("Поля:", "Fields:"), toolbar()), fields);
  const queryPane = el("section", { class: "dcs-query-pane" },
    el("div", { class: "dcs-query-heading" }, tr("Запрос:", "Query:"), button(tr("Конструктор запроса…", "Query wizard…"))), query,
    el("div", { class: "dcs-autofill" }, check("Автозаполнение", "Autofill", false), check("Использовать группировки запроса если возможно", "Use query grouping if applicable", false)));
  const detail = el("section", { class: "dcs-set-detail" });
  splitPane(detail, fieldPane, queryPane, "horizontal", tr("Высота полей набора", "Data set fields height"), 58);
  pages[0].classList.add("dcs-data-page");
  splitPane(pages[0], left, detail, "vertical", tr("Ширина наборов данных", "Data sets width"), 23);
  function selectSet(set, row) {
    tree.querySelectorAll("[role=treeitem]").forEach((n) => n.setAttribute("aria-selected", String(n === row)));
    replaceText(queryView, set.query || "");
    const flags = detail.querySelectorAll(".dcs-autofill input");
    flags[0].checked = set.auto_fill ?? true;
    flags[1].checked = set.use_query_group ?? true;
    const body = fields.querySelector("tbody");
    fill(body, (set.fields || []).slice(0, 300).flatMap((f) => [
      el("tr", null, el("td", { rowspan: "2" }, f.field || f.path), el("td", null, f.path), ...Array.from({ length: 9 }, () => el("td"))),
      el("tr", null, el("td", null, f.title), ...Array.from({ length: 9 }, () => el("td"))),
    ]));
  }
  allSets.forEach(({ set, depth }, i) => {
    const row = el("button", { type: "button", role: "treeitem", "data-browse-row": "", tabindex: "-1", title: set.name, style: `padding-left:${22 + depth * 16}px`, onclick: () => selectSet(set, row) }, icon("pic/DataCompositionNewTable"), set.name);
    tree.append(row); if (!i) selectSet(set, row);
  });
  browseRows(tree);
  const allFields = allSets.flatMap(({ set }) => set.fields || []);
  pages[3].classList.add("dcs-resources-page");
  pages[3].append(table("GridTotalsFields", [...new Set(allFields.map((f) => f.path))].sort().map((p) => [p])),
    el("div", { class: "dcs-transfer" }, [">", ">>", "<", "<<"].map((s) => button(s))),
    table("GridTotalsExpression", (schema.resources || []).map((r) => [r.path, r.expression, (r.groups || []).join(", ")]), source.get("GridTotalsExpression").columns.slice(0, 3)));
  pages[4].classList.add("dcs-table-page");
  const mark = (value) => value === true ? "✓" : "";
  pages[4].append(toolbar(), table("GridParams", (schema.parameters || []).map((p) => [
    p.name, p.title, (p.value_types || []).map((type) => en() ? type : type.replace(/^CatalogRef\./, "СправочникСсылка.")).join(", "), "", mark(p.value_list_allowed), p.typed_value ? valueText(p.typed_value) : p.value, p.expression,
    p.functional_option, mark(p.available_as_field), mark(p.use_restriction), mark(p.deny_incomplete),
    p.usage === "Auto" ? tr("Авто", "Auto") : p.usage, (p.input_parameters || []).join(", "),
  ])));
  pages[1].classList.add("dcs-table-page");
  pages[1].append(toolbar(), table("GridLinks", (schema.links || []).map((l) => [l.source, l.destination, l.source_expression, l.destination_expression, l.parameter, "", "", "", ""])));
  pages[2].classList.add("dcs-table-page");
  pages[2].append(toolbar(), table("GridExpressions", (schema.calculated_fields || []).map((f) => [f.path, f.expression, f.title]), [col("Путь", "Path", 180), col("Выражение", "Expression", 240), col("Заголовок", "Title", 180)]));
  pages[6].classList.add("dcs-table-page");
  pages[6].append(toolbar(), table("GridNestedReports"), el("div", { class: "dcs-settings-note" }, tr("Вложенные схемы недоступны в текущем представлении.", "Nested schemas are unavailable in this projection.")));

  pages[5].classList.add("dcs-templates-page");
  pages[5].append(el("section", { class: "dcs-table-page" }, toolbar(), table("GridTemplate")),
    el("section", { class: "dcs-template-detail" }, table("Moxel"), el("span", null, tr("Параметры макета:", "Template parameters:")), table("GridTemptaleParams")));
  const releaseSettings = mountDcsSettings(pages[7], schema, allFields, { tabs, table, col, toolbar, tr, source });

  ctx.body.querySelector("[role=tab]")?.focus({ preventScroll: true });
  return { destroy() { releaseSettings(); releaseEditor(queryView); } };
}
