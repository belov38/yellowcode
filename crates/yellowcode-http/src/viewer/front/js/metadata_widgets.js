import { el, fill } from "./dom.js";
import { icon } from "./icon.js";
import { openWindow } from "./tabs.js";
import { openMetadata } from "./metadata_open.js";

export function metadataWidgets(get) {
  const en = document.documentElement.lang === "en";
  const tr = (ru, english) => en ? english : ru;
  const short = (text) => String(text || "").split(".").pop();
  const enumText = (p) => {
    const labels = { Allow: "Разрешить", Deny: "Запретить", Use: "Использовать", DontUse: "Не использовать", Auto: "Авто", Begin: "Начало", Anywhere: "Любая часть", Directly: "Непосредственно", HierarchyFoldersAndItems: "Иерархия групп и элементов", HierarchyItems: "Иерархия элементов", ToItems: "Элементам", ToFolders: "Группам", ToFoldersAndItems: "Группам и элементам", Year: "В пределах года", Month: "В пределах месяца", Day: "В пределах дня", Nonperiodical: "Непериодическая", AutoFill: "Заполнять автоматически", AutoDelete: "Удалять автоматически", AutoDeleteOnUnpost: "Удалять автоматически", Managed: "Управляемый", Variable: "Переменная", Fixed: "Фиксированная" };
    return en ? p.text : labels[p.value] || p.text;
  };
  const field = (name, label) => {
    const p = get(name);
    if (!p) return null;
    const title = label || p.title;
    if (typeof p.value === "boolean") return el("label", { class: "metadata-check", "data-name": name },
      el("span", null, title), el("input", { type: "checkbox", checked: p.value, disabled: true, "aria-label": title }));
    const input = el("input", { class: "object-field", readonly: "", value: p.view === "enum" ? enumText(p) : p.value?.ref !== undefined ? short(p.value.ref) : p.value?.fields ? p.value.fields.map((name) => { const key = short(name); return en ? key : ({ Number: "Номер", Date: "Дата", Ref: "Ссылка", Description: "Наименование", Code: "Код" }[key] || key); }).join(", ") : p.text, "aria-label": title, title: p.tooltip || title });
    return el("label", { class: "metadata-prop", "data-name": name }, el("span", null, title + ":"),
      el("span", { class: "metadata-value" + (["enum", "md-ref", "list", "button"].includes(p.view) ? " metadata-combo" : "") }, input,
        ["enum", "md-ref", "list", "button"].includes(p.view) ? el("span", { "aria-hidden": "true" }, p.view === "enum" ? "▾" : "…") : null));
  };
  const command = (caption, open) => el("button", { class: "btn metadata-command", type: "button", disabled: !open,
    title: open ? caption : tr("Просмотр метаданных без редактирования", "Read-only metadata"), onclick: open }, caption);
  const caption = (it) => el("div", { "data-control": it.control }, command(it.caption, it.open ? () => openWindow({ kind: "module", path: it.open.path, module: it.open.module, icon: "module:" + it.open.module }) : null));
  const toolbar = () => el("div", { class: "metadata-toolbar", role: "toolbar", "aria-label": tr("Команды списка", "List actions") },
    [["CreateListItem", "Добавить", "Add"], ["Change", "Изменить", "Edit"], ["Delete", "Удалить", "Delete"], ["MoveUp", "Выше", "Move up"], ["MoveDown", "Ниже", "Move down"], ["Properties", "Свойства", "Properties"]].map(([pic, ru, english]) =>
      el("button", { type: "button", disabled: true, title: tr(ru, english), "aria-label": tr(ru, english) }, icon("pic/" + pic))));
  const row = (it, depth = 0, select) => {
    const open = () => openMetadata(it);
    return el("button", { class: "metadata-tree-row", type: "button", "data-path": it.path, style: "padding-left:" + (5 + depth * 14) + "px", onclick: (ev) => {
      const body = ev.currentTarget.closest(".metadata-sheet-body");
      body?.querySelectorAll(".cur").forEach((n) => { n.classList.remove("cur"); n.setAttribute("aria-pressed", "false"); });
      ev.currentTarget.setAttribute("aria-pressed", "true");
      ev.currentTarget.classList.add("cur");
      if (select) select(it);
    }, "aria-pressed": "false", ondblclick: select ? undefined : open, onkeydown: (ev) => { if (ev.key === "Enter") { ev.preventDefault(); if (select) ev.currentTarget.click(); else open(); } } }, it.icon ? icon(it.icon) : null, el("span", null, it.name || it.label));
  };
  const sheet = (title, nodes, tools = false, externalHeading = false) => el("div", { class: "metadata-sheet" },
    externalHeading && title ? el("div", { class: "metadata-tree-title" }, title) : null, tools ? toolbar() : null,
    el("div", { class: "metadata-sheet-body" }, !externalHeading && title ? el("div", { class: "metadata-tree-title" }, title) : null, nodes));
  const radioGroup = (title, property, values) => el("fieldset", { class: "metadata-radio-group" }, el("legend", null, title), values.map(([value, text]) =>
    el("label", { class: "metadata-radio" }, el("input", { type: "radio", checked: property?.value === value, disabled: true }), text)));
  const checkRow = (node, checked) => el("label", { class: "metadata-membership", "data-path": node.path },
    el("input", { type: "checkbox", checked, disabled: true, "aria-label": node.label || node.name }), node.icon ? icon(node.icon) : null, el("span", null, node.label || node.name));
  const treeGroup = (title, children, expanded = true) => el("details", { class: "metadata-tree-group", open: expanded ? "" : null }, el("summary", null, title), children);
  const standardAttributes = (property) => {
    const previous = document.activeElement;
    const title = tr("Стандартные реквизиты", "Standard attributes");
    const dialog = el("dialog", { class: "service-dialog metadata-standard", "aria-label": title });
    const details = el("div", { class: "metadata-standard-properties" });
    const attrs = property.value?.standard_attributes || [];
    const list = sheet(null, attrs.map((attr) => row({ name: attr.name }, 0, () => {
      const widgets = metadataWidgets((name) => attr.props.find((p) => p.name === name));
      fill(details, attr.props.map((p) => widgets.field(p.name)));
    })));
    dialog.append(el("header", { class: "dialog-caption" }, title, el("button", { type: "button", onclick: () => dialog.close(), "aria-label": tr("Закрыть", "Close") }, "×")),
      el("div", { class: "metadata-standard-body" }, list, details));
    dialog.addEventListener("close", () => { dialog.remove(); if (previous?.isConnected) previous.focus(); });
    document.body.append(dialog); dialog.showModal(); list.querySelector("button")?.click();
  };
  return { short, field, command, caption, toolbar, row, sheet, radioGroup, checkRow, treeGroup, standardAttributes };
}
