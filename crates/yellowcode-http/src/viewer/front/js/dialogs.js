import { mountDcsRenderer } from "./dcs.js";
import { el, fill } from "./dom.js";
import { api } from "./api.js";
import { icon } from "./icon.js";
import { windows, activate, activeWindow, closeWindow } from "./tabs.js";
import { skinItems } from "./skin.js";
import { editorSettings as currentEditorSettings, setLineNumbers } from "./editor.js";

const actions = new Map();
const defaultEditorSettings = {
  get() {
    const style = getComputedStyle(document.documentElement);
    return { ...currentEditorSettings(), fontFamily: style.getPropertyValue("--cfg-font-mono").trim() || "Courier New",
      fontSize: parseFloat(style.getPropertyValue("--cfg-font-mono-size")) || 13 };
  },
  set(value) {
    const style = document.documentElement.style;
    style.setProperty("--cfg-font-mono", value.fontFamily);
    style.setProperty("--cfg-font-mono-size", value.fontSize + "px");
    style.setProperty("--cfg-mono-line", Math.ceil(value.fontSize * 1.2) + "px");
    setLineNumbers(value.lineNumbers);
  },
};
let editorSettings = defaultEditorSettings;
let catalogPromise;
const en = () => document.documentElement.lang === "en";
const tr = (ru, english) => en() ? english : ru;
const caption = (value) => (value?.[en() ? "en" : "ru"] || value?.ru || value?.en || "").replaceAll("&", "");
const unavailable = () => tr("Не реализовано в Yellowcode", "Not implemented in Yellowcode");
const catalog = () => catalogPromise ||= fetch("/front/dialogs.json").then((r) => {
  if (!r.ok) throw new Error("dialogs.json: " + r.status);
  return r.json();
});

export const dialogCommands = {
  IDS_CMD_OPTIONS: "options", IDS_CMD_ALL_WINDOWS: "windows", IDS_CMD_HELPABOUT: "about",
  IDS_CMD_COMPARECONFIGS: "compare", IDS_CMD_OPENPARENTCONFIGS: "support",
  IDS_CMD_CONFIGQUALITYTEST: "check", IDS_CMD_MDTEXTOUTPUT: "report",
  IDS_CMD_DEPLOYWEBIB: "publish", IDS_CMD_USERLIST: "users", IDS_CMD_IBADMPARAMS: "ib",
};

export function registerDialogHandler(form, name, handler) {
  const key = form + "/" + name;
  actions.set(key, handler);
  return () => { if (actions.get(key) === handler) actions.delete(key); };
}

export function registerEditorSettings(adapter) { editorSettings = adapter || defaultEditorSettings; }

function disable(node) {
  node.title = unavailable();
  node.setAttribute("aria-disabled", "true");
  if ("disabled" in node) node.disabled = true;
  for (const child of node.querySelectorAll("input,button,textarea,select")) child.disabled = true;
}

function geometry(node, g) {
  Object.assign(node.style, { left: g.left + "px", top: g.top + "px", width: Math.max(0, g.width) + "px", height: Math.max(0, g.height) + "px" });
  return node;
}

function shell(key, title, width, height) {
  const previous = document.activeElement;
  const d = el("dialog", { class: "service-dialog", "data-dialog": key, "aria-label": title });
  const body = el("div", { class: "dialog-body", style: `width:${width}px;height:${height}px` });
  const error = el("div", { class: "dialog-error", role: "alert", hidden: true });
  const close = () => d.close();
  const chrome = el("span", { class: "dialog-caption-buttons" });
  for (const symbol of key === "users" ? ["_", "□"] : key === "support" ? ["□"] : []) {
    const button = el("button", { type: "button" }, symbol); disable(button); chrome.append(button);
  }
  d.append(el("header", { class: "dialog-caption" }, el("strong", null, title), chrome,
    el("button", { type: "button", "data-dialog-close": "", "aria-label": tr("Закрыть", "Close"), onclick: close }, "×")), error, body);
  const context = { dialog: d, body, close, values: {}, nodes: new Map(), schema: null,
    report: (e) => { error.hidden = false; error.textContent = e.message || String(e); } };
  context.run = (fn) => { try { Promise.resolve(fn(context)).catch(context.report); } catch (e) { context.report(e); } };
  d.addEventListener("close", () => { d.remove(); if (previous?.isConnected) previous.focus(); });
  document.body.append(d);
  d.showModal();
  return context;
}

function tabs(pages, parent) {
  const bar = el("div", { class: "dialog-tabs", role: "tablist" });
  const bodies = pages.map(() => el("div", { class: "dialog-page", role: "tabpanel" }));
  const buttons = pages.map((p, i) => el("button", { type: "button", role: "tab", onclick: () => select(i) }, caption(p.caption)));
  function select(index) {
    bodies.forEach((b, i) => { b.hidden = i !== index; });
    buttons.forEach((b, i) => { b.setAttribute("aria-selected", String(i === index)); b.tabIndex = i === index ? 0 : -1; });
  }
  bar.append(...buttons);
  bar.addEventListener("keydown", (e) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    e.preventDefault();
    const current = buttons.indexOf(document.activeElement);
    const next = e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : (current + (e.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
    select(next); buttons[next].focus();
  });
  parent.append(bar, ...bodies);
  select(0);
  return bodies;
}

function grid(c) {
  const cols = c.columns || [];
  return el("div", { class: "dialog-grid", tabindex: "0" }, el("table", null,
    el("thead", null, el("tr", null, cols.map((col) => el("th", { style: `min-width:${Math.max(35, Number(col.width_serialized) || 90)}px` }, caption(col.caption))))), el("tbody")));
}

function renderControls(controls, parent, form, ctx) {
  for (const c of controls) {
    const label = caption(c.caption);
    let node;
    const handler = actions.get(form + "/" + c.name);
    switch (c.type) {
      case "panel": {
        node = el("div", { class: "dialog-panel" });
        if (c.tab_pages.length > 1) {
          const bodies = tabs(c.tab_pages, node);
          c.tab_pages.forEach((p, i) => renderControls(c.children.filter((child) => child.page_index === p.index), bodies[i], form, ctx));
        } else renderControls(c.children, node, form, ctx);
        break;
      }
      case "group": node = el("fieldset", null, label ? el("legend", null, label) : null); break;
      case "label": node = el("span", null, /^<<.*>>$/.test(label) ? "" : label); break;
      case "button": {
        const closes = /^(ОК|OK|Отмена|Cancel|Закрыть|Close)$/i.test(label);
        node = el("button", { type: "button", onclick: () => handler ? ctx.run(handler) : closes && ctx.close() }, label);
        if (!handler && !closes) disable(node);
        if (/^(ОК|OK)$/i.test(label)) node.dataset.dialogDefault = "";
        break;
      }
      case "checkbox": case "radio":
        node = el("label", null, el("input", { type: c.type, name: form + "/" + c.name, checked: c.checked }), label);
        if (!handler) disable(node);
        break;
      case "input": case "file_input": case "font": case "reference": case "color":
        node = el("input", { type: c.type === "color" ? "color" : /password|pswd/i.test(c.name) ? "password" : "text", "aria-label": label || c.name });
        if (!handler) disable(node);
        break;
      case "choice": node = el("select", { "aria-label": label || c.name }, el("option", null, typeof c.value === "object" ? caption(c.value) : c.value)); if (!handler) disable(node); break;
      case "text": node = el("textarea", { "aria-label": label || c.name }); if (!handler) disable(node); break;
      case "list": node = form === "windows" ? el("div", { role: "listbox", tabindex: "0", "aria-multiselectable": "true", "aria-label": tr("Окна", "Windows") }) : el("select", { size: "8", "aria-label": label || c.name }); if (!handler) disable(node); break;
      case "grid": case "table": case "spreadsheet": node = grid(c); if (!handler) disable(node); break;
      case "command_bar": node = el("div", { class: "dialog-commandbar" }); disable(node); break;
      case "picture": node = el("div", { class: "dialog-picture" }, icon("tree/Configuration")); break;
      case "splitter": node = el("div", { class: "dialog-splitter", role: "separator" }); break;
      case "indicator": node = el("progress", { max: "1", value: "0", "aria-label": label || c.name }); break;
      case "preview": node = el("div", { class: "dialog-preview" }, "Aa Бб 0123"); disable(node); break;
      default: throw new Error("Unknown dialog control: " + c.type);
    }
    if (c.value !== undefined) node.value = typeof c.value === "object" ? caption(c.value) : c.value;
    node.classList.add("dialog-control", "dialog-" + c.type);
    if (c.observed_disabled) node.classList.add("dialog-observed-disabled");
    node.dataset.control = c.name;
    node.dataset.controlType = c.type;
    ctx.nodes.set(form + "/" + c.name, node);
    if (handler && c.type !== "button") node.addEventListener("change", () => {
      ctx.values[c.name] = node.matches("label") ? node.querySelector("input").checked : node.value;
      ctx.run(handler);
    });
    parent.append(geometry(node, c.geometry));
  }
}

function windowList(ctx) {
  const list = ctx.nodes.get("windows/WindowList");
  const selected = new Set(activeWindow() ? [activeWindow().id] : []);
  const go = ctx.nodes.get("windows/BtnActivate");
  const close = ctx.nodes.get("windows/BtnClose");
  const sort = ctx.nodes.get("windows/Sort");
  let sorted = false;
  const enable = (node, value) => {
    node.disabled = !value;
    node.setAttribute("aria-disabled", String(!value));
    node.title = value ? "" : unavailable();
  };
  function paint() {
    let wins = windows();
    if (sorted) wins = [...wins].sort((a, b) => a.title.localeCompare(b.title));
    for (const id of selected) if (!wins.some((win) => win.id === id)) selected.delete(id);
    if (!selected.size && wins.length) selected.add(wins[0].id);
    fill(list, wins.map((win) => el("button", {
      type: "button", class: "dialog-window-row", role: "option", tabindex: "-1", "data-window-id": String(win.id),
      "aria-selected": String(selected.has(win.id)),
      onclick: (e) => {
        if (!(e.ctrlKey || e.metaKey)) selected.clear();
        if (selected.has(win.id)) selected.delete(win.id); else selected.add(win.id);
        paint();
      },
      ondblclick: () => { activate(win); ctx.close(); },
    }, icon(win.iconKey), el("span", null, win.title))));
    enable(go, selected.size === 1);
    enable(close, selected.size > 0);
  }
  list.removeAttribute("aria-disabled"); list.removeAttribute("title");
  go.addEventListener("click", () => { activate(windows().find((win) => selected.has(win.id))); ctx.close(); });
  close.addEventListener("click", () => {
    for (const win of windows()) if (selected.has(win.id)) closeWindow(win);
    paint();
  });
  const input = sort.querySelector("input"); input.disabled = false;
  sort.removeAttribute("aria-disabled"); sort.removeAttribute("title");
  input.addEventListener("change", () => { sorted = input.checked; paint(); });
  list.addEventListener("keydown", (event) => {
    const wins = windows();
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
      event.preventDefault(); wins.forEach((win) => selected.add(win.id)); paint(); return;
    }
    if (event.key === "Enter" && selected.size === 1) { event.preventDefault(); go.click(); return; }
    if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key) || !wins.length) return;
    event.preventDefault();
    const rows = [...list.querySelectorAll("[role=option]")];
    const current = rows.findIndex((row) => selected.has(Number(row.dataset.windowId)));
    const next = event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1 : Math.max(0, Math.min(rows.length - 1, current + (event.key === "ArrowDown" ? 1 : -1)));
    selected.clear(); selected.add(Number(rows[next].dataset.windowId)); paint();
    list.querySelector('[aria-selected=true]')?.scrollIntoView({ block: "nearest" });
  });
  paint();
}

function editorPage(parent, ctx) {
  const initial = editorSettings?.get() || { fontFamily: 'Courier New', fontSize: 13, lineNumbers: false };
  const font = el("input", { value: initial.fontFamily, "aria-label": tr("Шрифт", "Font") });
  const size = el("input", { type: "number", min: "8", max: "40", value: String(initial.fontSize), "aria-label": tr("Размер", "Size") });
  const lines = el("input", { type: "checkbox", checked: initial.lineNumbers });
  const group = el("fieldset", { class: "dialog-editor-settings" }, el("legend", null, tr("Редактор Yellowcode", "Yellowcode editor")),
    el("label", null, tr("Шрифт: ", "Font: "), font), el("label", null, tr("Размер: ", "Size: "), size),
    el("label", null, lines, tr("Номера строк", "Line numbers")));
  if (!editorSettings) disable(group);
  parent.append(group);
  ctx.applyEditor = () => editorSettings?.set({ fontFamily: font.value, fontSize: Math.max(8, Math.min(40, Number(size.value) || 13)), lineNumbers: lines.checked });
}

async function options(data) {
  const ctx = shell("options", caption(data.forms.options.title), 519, 660);
  const panel = el("div", { class: "dialog-options-pages", "data-control": "Pages", "data-control-type": "panel" });
  const bodies = tabs(data.option_pages, panel);
  ctx.body.append(panel);
  const pageIndex = (key) => data.option_pages.findIndex((p) => p.form === key);
  data.option_pages.forEach((p, i) => renderControls(data.forms[p.form].controls, bodies[i], p.form, ctx));
  panel.querySelector(".dialog-tabs").classList.add("dialog-options-tabs");
  panel.querySelectorAll(":scope > .dialog-tabs > button")[pageIndex("general")].click();
  editorPage(bodies[pageIndex("text")], ctx);
  const skins = await skinItems();
  if (!ctx.dialog.open) return ctx.dialog;
  const select = el("select", { "aria-label": tr("Скин", "Skin") });
  const end = skins.findIndex((s) => s.sep);
  const choices = skins.slice(0, end < 0 ? skins.length : end).filter((s) => s.onclick);
  choices.forEach((s, i) => select.append(el("option", { value: String(i), selected: s.label.startsWith("✓") ? "" : null }, s.label.replace(/^✓ /, ""))));
  let chosen = null, editorDirty = false;
  const applyButton = el("button", { type: "button", disabled: true }, tr("Применить", "Apply"));
  disable(applyButton);
  const dirty = () => { applyButton.disabled = false; applyButton.removeAttribute("title"); applyButton.removeAttribute("aria-disabled"); };
  select.addEventListener("change", () => { chosen = choices[Number(select.value)]; dirty(); });
  bodies[pageIndex("general")].append(el("label", { class: "dialog-skin-settings" }, tr("Скин: ", "Skin: "), select));
  const editorGroup = bodies[pageIndex("text")].querySelector(".dialog-editor-settings");
  for (const event of ["input", "change"]) editorGroup.addEventListener(event, () => { editorDirty = true; dirty(); });
  const apply = async () => {
    if (editorDirty) await ctx.applyEditor();
    if (chosen) await chosen.onclick();
    editorDirty = false; chosen = null; disable(applyButton);
  };
  applyButton.addEventListener("click", () => ctx.run(apply));
  const footer = el("div", { class: "dialog-footer" },
    el("button", { type: "button", "data-dialog-default": "", onclick: () => ctx.run(async () => { await apply(); ctx.close(); }) }, "OK"),
    el("button", { type: "button", onclick: ctx.close }, tr("Отмена", "Cancel")), applyButton);
  const help = el("button", { type: "button" }, tr("Справка", "Help")); disable(help); footer.append(help);
  [...footer.children].forEach((button, i) => { button.dataset.control = ["OkBtn", "CancelBtn", "ApplyBtn", "HelpBtn"][i]; button.dataset.controlType = "button"; });
  ctx.body.append(footer);
  return ctx.dialog;
}

async function about() {
  const info = await api("info");
  const ctx = shell("about", tr("О программе", "About"), 474, 463);
  ctx.body.classList.add("dialog-about");
  ctx.body.append(el("div", { class: "dialog-about-mark" }, "Y"),
    el("div", { class: "dialog-about-product" }, el("p", null, "Yellowcode " + (info.yellowcode_version || "")),
      el("p", { class: "dialog-about-config" }, tr("Конфигурация:", "Configuration:")),
      el("p", { "data-about-configuration": "" }, (info.synonym || info.name) + (info.configuration_version ? " (" + info.configuration_version + ")" : "")),
      el("p", { class: "dialog-about-usage" }, tr("Использование конфигурации:", "Configuration use:")),
      el("p", null, tr("Просмотр конфигурации", "Configuration viewer"))),
    el("dl", null, el("dt", null, tr("Наименование:", "Name:")), el("dd", null, info.name),
      el("dt", null, tr("Режим:", "Mode:")), el("dd", null, tr("Просмотр", "Viewer")),
      el("dt", null, tr("Каталог:", "Directory:")), el("dd", null, (info.source || "").replace(/[/\\][^/\\]+\.(cf|cfe|1cd)$/i, "")),
      el("dt", null, tr("Приложение:", "Application:")), el("dd", null, tr("Конфигуратор", "Designer")),
      el("dt", null, tr("Пользователь:", "User:")), el("dd"),
      el("dt", null, tr("Локализация:", "Locale:")), el("dd", null, en() ? "English" : "Русский"),
      el("dt", null, tr("Лицензия:", "License:")), el("dd", null, "MIT OR Apache-2.0")),
    el("a", { class: "dialog-licenses", href: "/front/NOTICE", target: "_blank", rel: "noopener" }, tr("Лицензии используемых компонентов", "Component licenses")),
    el("div", { class: "dialog-footer" }, el("button", { type: "button", "data-dialog-default": "", onclick: ctx.close }, "OK")));
  return ctx.dialog;
}

export async function mountDcs(win, schema) {
  const data = await catalog();
  if (win.closed) return;
  return mountDcsRenderer(win, schema, data, { tabs, grid, disable, tr, en });
}

export async function openDialog(key, context = {}) {
  if (key === "about") return about();
  const data = await catalog();
  if (key === "options") return options(data);
  const form = data.forms[key];
  if (!form) throw new Error("Unknown dialog: " + key);
  const ctx = shell(key, caption(form.title) || tr("Список пользователей", "Users"), form.size.width, form.size.height);
  ctx.schema = context.schema || null;
  renderControls(form.controls, ctx.body, key, ctx);
  if (key === "windows") windowList(ctx);
  if (key === "users") {
    ctx.nodes.get("users/TotalUsersCntSt").textContent = "0";
    const toolbar = el("div", { class: "dialog-users-toolbar" });
    const details = () => ctx.run(() => openDialog("user"));
    const menu = el("div", { class: "dialog-users-actions", hidden: true },
      el("button", { type: "button", onclick: () => { menu.hidden = true; details(); } }, tr("Добавить…", "Add…")));
    toolbar.append(el("button", { type: "button", onclick: () => { menu.hidden = !menu.hidden; } }, tr("Действия ▾", "Actions ▾")), menu);
    for (const [pic, ru, english, run] of [
      ["CreateListItem", "Добавить", "Add", details], ["CloneObject", "Скопировать", "Copy"],
      ["Change", "Изменить", "Edit"], ["Delete", "Удалить", "Delete"],
      ["FilterAndSort", "Отбор", "Filter"], ["FilterByCurrentValue", "Отбор по текущему значению", "Filter by current value"],
      ["ClearFilter", "Отключить отбор", "Clear filter"], ["Refresh", "Обновить", "Refresh"],
    ]) {
      const handler = actions.get("users/" + pic) || run;
      const button = el("button", { type: "button", "aria-label": tr(ru, english), title: tr(ru, english), onclick: handler ? () => ctx.run(handler) : null }, icon("pic/" + pic));
      ctx.nodes.set("users/" + pic, button);
      if (!handler) disable(button);
      toolbar.append(button);
    }
    ctx.body.append(toolbar);
  }
  ctx.dialog.querySelector("[data-dialog-default]")?.focus({ preventScroll: true });
  ctx.dialog.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.target.closest("textarea,button,select")) {
      const button = ctx.dialog.querySelector("[data-dialog-default]:not(:disabled)");
      if (button) { e.preventDefault(); button.click(); }
    }
  });
  return ctx.dialog;
}
