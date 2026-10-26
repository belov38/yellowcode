import { $, el, fill } from "./dom.js";
import { T } from "./i18n.js";
import { show } from "./api.js";
import { icon } from "./icon.js";
import { addMenu, initMenus, showContext, closeAllMenus } from "./menu.js";
import { activeWindow, windows, closeWindow, closeAll, activate, openWindow, onActive } from "./tabs.js";
import { selectedTreeRow, selectTreeRow, revealInTree } from "./tree.js";
import { setPaletteTarget } from "./props.js";
import { isTemplate, openMetadata } from "./metadata_open.js";
import { HELPER_PATH } from "./helper.js";
import { conversionItems } from "./convert.js";
import { desktopItems, recentItems, openSource, desktopBusy } from "./desktop.js";
import { skinItems } from "./skin.js";
import { dialogCommands, openDialog } from "./dialogs.js";
import { openQuery, openCode, dataTreeItems } from "./x/index.js";

const registry = new Map();
const serviceTools = new Map();
export function registerServiceTool(key, tool) {
  serviceTools.set(key, tool);
  return () => serviceTools.delete(key);
}
registerServiceTool("query-console", { label: () => T.queryConsole, shortcut: "Ctrl+Shift+Q", run: openQuery });
registerServiceTool("code-console", { label: () => T.codeConsole, shortcut: "Ctrl+Shift+E", run: openCode });
let treeDataPath = null;
let treeData = null;
let treeDataRequest = null;
let layouts = null;
let onError = () => {};
const mac = /Mac|iPhone|iPad/.test(navigator.platform);
const label = (item) => (item[document.documentElement.lang === "en" ? "en" : "ru"] || item.ru || item.dynamic_caption || item.key || "")
  .replaceAll("%CTRL%", mac ? "Cmd" : "Ctrl").replaceAll("%ALT%", "Alt");
const shortcut = (item) => [item.shortcut, ...(item.shortcut_alt || [])].filter(Boolean).join(", ").replaceAll("Ctrl", mac ? "Cmd" : "Ctrl");
const moduleWindow = () => activeWindow()?.kind === "module" ? activeWindow() : null;
const controller = () => moduleWindow()?.ctl;
const moduleReady = () => !!controller()?.ready();
const treePath = () => selectedTreeRow()?.dataset.path || null;

function register(key, run, enabled = () => true, opensDialog = false) {
  for (const command of allCommands()) if (command.key === key) registry.set(command.id, { run, enabled, opensDialog });
}
function allCommands() {
  const result = [];
  function visit(value) {
    if (!value || typeof value !== "object") return;
    if (value.type === "command") result.push(value);
    for (const child of Object.values(value)) if (typeof child === "object") visit(child);
  }
  visit(layouts);
  return result;
}
function handler(command) {
  if (command.key === "IDS_CMD_PURIFYOPEN") return registry.get(allCommands().find(c => c.key === "IDS_CMD_OPEN")?.id);
  return registry.get(command.id);
}
function enabled(command) {
  const action = handler(command);
  return !command.unresolved && (command.effect === "read" || action?.opensDialog) && !!action?.enabled();
}
function execute(command) {
  if (!enabled(command)) return;
  try { Promise.resolve(handler(command).run()).catch(onError); } catch (error) { onError(error); }
  refreshCommands();
}
function item(command) {
  if (command.type === "separator") return { sep: true };
  const children = command.items;
  const disabled = command.unresolved ||
    (children ? !children.length || ["edit", "run"].includes(command.effect) : !enabled(command));
  return {
    id: command.id, key: command.key, effect: command.effect, label: label(command), icon: command.icon,
    unresolved: command.unresolved, bold: command.default_bold, checked: command.checked, observedDisabled: command.observed_disabled,
    shortcut: shortcut(command), disabled,
    title: disabled ? T.notImplemented : label(command),
    items: children ? children.map(item) : null, onclick: () => execute(command),
  };
}
function button(command) {
  const b = el("button", { type: "button", "data-command": command.id, "data-key": command.key, "data-effect": command.effect,
    onclick: () => execute(command) });
  if (command.icon) b.appendChild(icon(command.icon));
  else b.textContent = label(command);
  return b;
}
function toolbar(items, title) {
  return el("div", { class: "toolbar", role: "toolbar", "aria-label": title }, items.map((c) =>
    c.type === "separator" ? el("span", { class: "toolbar-separator" }) : button(c)));
}
function dialog(title, content) {
  document.getElementById("command-dialog")?.remove();
  const d = el("dialog", { id: "command-dialog" }, el("h3", null, title), content,
    el("button", { type: "button", onclick: () => d.close() }, T.closeDialog));
  d.addEventListener("close", () => d.remove());
  document.body.appendChild(d);
  d.showModal();
}
function windowList() { return openDialog("windows"); }

function cycle(delta) {
  const list = windows();
  activate(list[(list.indexOf(activeWindow()) + list.length + delta) % list.length]);
}
function sortPalette(alpha) {
  $("opt-alpha").checked = alpha;
  $("opt-alpha").dispatchEvent(new Event("change"));
}
function installHandlers() {
  register("IDS_CMD_FILEOPEN", () => openSource(), () => !!openSource && !desktopBusy());
  for (const key of ["IDS_CMD_FILECLOSE", "IDS_CMD_CLOSEWINDOW"])
    register(key, () => closeWindow(activeWindow()), () => !!activeWindow());
  register("IDS_CMD_CLOSEALL", closeAll, () => windows().length > 0);
  for (const key of ["IDS_CMD_WINDOWLIST", "IDS_CMD_WINDOWS", "IDS_CMD_ALL_WINDOWS"]) register(key, windowList);
  register("IDS_MDI_CMD_NEXTWINDOW", () => cycle(1), () => windows().length > 1);
  register("IDS_MDI_CMD_PREVWINDOW", () => cycle(-1), () => windows().length > 1);
  register("IDS_CMD_PROPERTY", () => {
    document.body.classList.remove("no-palette");
    if (treePath()) setPaletteTarget(treePath());
    if (isTemplate(treePath())) openWindow({ kind: "object", path: treePath(), icon: selectedTreeRow()?.dataset.icon });
  });
  register("IDS_CMD_FINDOBJECT", () => revealInTree(activeWindow().path), () => !!activeWindow());
  register("IDS_CMD_FINDINTEXTS", () => $("search").focus());
  register("IDS_CMD_FIND", () => controller() ? controller().find() : $("search").focus());
  for (const key of ["IDS_CMD_FINDFORWARD", "IDS_CMD_FINDREPEAT"]) register(key, () => controller().findNext(), moduleReady);
  register("IDS_CMD_FINDBACK", () => controller().findPrevious(), moduleReady);
  register("IDS_CMD_GOTOLINE", () => {
    const input = el("input", { type: "number", min: "1", value: "1", "aria-label": T.gotoLine });
    const form = el("form", { onsubmit: (e) => { e.preventDefault(); controller()?.goTo(Number(input.value)); $("command-dialog").close(); } }, input, el("button", { type: "submit" }, T.gotoLine));
    dialog(T.gotoLine, form); input.focus();
  }, moduleReady);
  for (const [key, method] of Object.entries({ COLLAPSEBLOCK: "fold", EXPANDBLOCK: "unfold", COLLAPSEBLOCK_EX: "foldSub", EXPANDBLOCK_EX: "unfoldSub",
    COLLAPSEBLOCKALL: "foldAll", EXPANDBLOCKALL: "unfoldAll", REGENTQUERYEDIT: "builder", MODULEGOTOGLOBAL: "outline", ENTRYLIST: "outline", COPY: "copy", SELECTALL: "selectAll" }))
    register("IDS_CMD_" + key, () => controller()[method](), moduleReady);
  register("IDS_CMD_CHECKMODULE", () => controller().check(), () => moduleReady() && controller().canCheck());
  register("IDS_CMD_SYNTAXHELPER", () => openWindow({ kind: "helper", path: HELPER_PATH, title: T.helperItem }));
  for (const [key, form] of Object.entries(dialogCommands)) register(key, () => openDialog(form), () => true, true);
  register("IDS_PROPBROWSE_SORTBYALPHA", () => sortPalette(true));
  register("IDS_PROPBROWSE_SORTBYCAT", () => sortPalette(false));
  register("IDS_PROPBROWSE_SEARCH_BACK_TOOLTIP_HK", () => {
    const field = el("input", { type: "search", "aria-label": T.find, oninput: () => {
      for (const row of $("palette-body").querySelectorAll("tr")) row.hidden = !row.textContent.toLocaleLowerCase().includes(field.value.toLocaleLowerCase());
    } });
    dialog(T.find, field); field.focus();
    $("command-dialog").addEventListener("close", () => { for (const row of $("palette-body").querySelectorAll("tr")) row.hidden = false; });
  });
  register("IDS_CMD_OPEN", () => {
    const row = selectedTreeRow();
    openMetadata(row.dataset);
  }, () => !!treePath());
  const moduleKeys = {
    OPENMODULE: ["ObjectModule", "Module", "FormModule"], OPENMANAGERMODULE: ["ManagerModule"],
    OPENMNGCLIENTMODULE: ["ManagedApplicationModule"], OPENSESSIONMODULE: ["SessionModule"],
    OPENCOMMANDMODULE: ["CommandModule"], OPENSTORAGEMODULE: ["ValueManagerModule"],
    OPENGLOBALMODULE: ["OrdinaryApplicationModule"], OPENEXTERNALCONNECTIONMODULE: ["ExternalConnectionModule"],
    OPENCOMMONMODULE: ["Module"], OPENRECORDSETMODULE: ["RecordSetModule"],
  };
  for (const [key, modules] of Object.entries(moduleKeys)) {
    const target = () => treeDataPath === treePath() && modules.find(mod => treeData?.modules?.includes(mod));
    register("IDS_CMD_" + key, () => openWindow({ kind: "module", path: treePath(), module: target() }), () => !!target());
  }
  const formKeys = { MDOPENITEMFORM: "DefaultObjectForm", MDOPENGROUPFORM: "DefaultFolderForm",
    MDOPENLISTFORM: "DefaultListForm", MDOPENCHOICEFORM: "DefaultChoiceForm",
    MDOPENGROUPCHOICEFORM: "DefaultFolderChoiceForm", MDOPENRECORDFORM: "DefaultRecordForm",
    MDOPENDEFAULTVARIANTFORM: "DefaultReportForm", MDOPENDEFAULTSETTINGSFORM: "DefaultSettingsForm" };
  for (const [key, property] of Object.entries(formKeys)) {
    const target = () => {
      if (treeDataPath !== treePath()) return null;
      const path = treeData?.properties?.find(p => p.name === property)?.value?.ref;
      return treeData?.forms?.find(form => form.path === path)?.path;
    };
    register("IDS_CMD_" + key, () => openWindow({ kind: "form", path: target() }), () => !!target());
  }
  register("IDS_CMD_MDOPENFORM", () => openWindow({ kind: "form", path: treePath() }),
    () => ["form", "commonForm"].includes(treeContextKey()));
}

async function menuEntries(menu) {
  const entries = [];
  for (const command of menu.items) {
    if (command.key === "IDS_CMD_FILEOPEN" && openSource) {
      entries.push(item(command), ...(await desktopItems()), ...conversionItems());
    } else if (command.key === "IDS_CMD_RESENTFILE" && recentItems) {
      const recent = await recentItems();
      entries.push(...(recent.length ? recent : [item(command)]));
    } else entries.push(item(command));
  }
  if (menu.key === "IDS_BAND_SERVICE") {
    entries.push({ sep: true }, { label: T.skin, items: skinItems });
    if (serviceTools.size) entries.push({ sep: true }, ...Array.from(serviceTools, ([key, tool]) => ({
      key, label: tool.label(), shortcut: shortcut(tool), onclick: tool.run,
    })));
  }
  return entries;
}
export function treeContextKey(row = selectedTreeRow()) {
  const path = row?.dataset.path || "";
  const kind = row?.dataset.icon || "";
  if (path === "Configuration" || (!row && !path)) return "root";
  if (!path || kind.startsWith("folder:")) return "folder.Catalogs";
  if (path.startsWith("CommonForm.")) return "commonForm";
  if (kind.includes("Form") || /\.Form\./.test(path)) return "form";
  if (kind.includes("CommonModule") || row.dataset.module) return "commonModule";
  if (kind.includes("Attribute") || /\.(Attribute|Dimension|Resource|Column)\./.test(path)) return "attribute";
  const kinds = { Document: "document", InformationRegister: "informationRegister", Report: "report",
    DataProcessor: "dataProcessor", Enum: "enum" };
  return path.split(".").length === 2 ? kinds[path.split(".")[0]] || "catalog" : "catalog";
}
async function loadTreeData() {
  const path = treePath();
  if (treeDataPath !== path) {
    treeDataPath = path;
    treeData = null;
    treeDataRequest = path ? show(path).then(data => {
      if (treeDataPath === path) treeData = data;
    }) : Promise.resolve();
  }
  await treeDataRequest;
}
async function treeItems() {
  await loadTreeData();
  return [...layouts.tree_contexts[treeContextKey()].map(item), ...dataTreeItems(treePath())];
}
export function showObservedContext(key) {
  showContext({ preventDefault() {}, clientX: 180, clientY: 180 }, layouts.observed_contexts[key].map(item), { disabledHover: key === "editor.text" });
}
export function refreshCommands() {
  if (!layouts) return;
  const commands = new Map(allCommands().map((c) => [c.id, c]));
  for (const node of document.querySelectorAll("[data-command]")) {
    const command = commands.get(node.dataset.command);
    if (!command || command.items) continue;
    node.disabled = node.dataset.unresolved === "true" || !enabled(command);
    const tooltipLabel = label({ ru: command.tooltip_ru || command.ru, en: command.tooltip_en || command.en });
    const hint = tooltipLabel + (shortcut(command) ? " (" + shortcut(command) + ")" : "");
    if (node.closest(".toolbar, #tree-actions")) {
      node.dataset.tooltip = hint + (node.disabled ? "\n" + T.notImplemented : "");
      node.removeAttribute("title");
    } else node.title = node.disabled ? T.notImplemented : hint;
    node.setAttribute("aria-label", label(command));
  }
  $("menu-text").hidden = !moduleWindow();
  $("module-toolbar").hidden = !moduleWindow();
}
function shortcuts(event) {
  if (event.defaultPrevented || event.isComposing || event.target.closest("dialog")) return;
  if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey && ["a", "c", "v", "x", "z", "y", "f"].includes(event.key.toLowerCase())) return;
  if (event.target.closest("input,textarea,[contenteditable=true]") && !event.ctrlKey && !event.metaKey && !event.altKey) return;
  const match = (binding) => {
    const normalized = binding.replace("Num+", "NumpadAdd");
    const parts = normalized.split("+"), key = parts.pop();
    return event.ctrlKey === (!mac && parts.includes("Ctrl")) && event.metaKey === (mac && parts.includes("Ctrl")) &&
      event.altKey === parts.includes("Alt") && event.shiftKey === parts.includes("Shift") &&
      (key.startsWith("Num") ? event.code === ({ "Num-": "NumpadSubtract", NumpadAdd: "NumpadAdd", "Num/": "NumpadDivide" })[key] :
        event.key.toLowerCase() === ({ Del: "delete", Ins: "insert", Up: "arrowup", Down: "arrowdown" }[key] || key.toLowerCase()));
  };
  if ((mac ? event.metaKey : event.ctrlKey) && event.altKey && !event.shiftKey && event.key.toLowerCase() === "m") { event.preventDefault(); $("tree-filter").focus(); return; }
  for (const tool of serviceTools.values()) {
    if (tool.shortcut && match(tool.shortcut)) { event.preventDefault(); tool.run(); return; }
  }
  const command = allCommands().find((c) => enabled(c) && [c.shortcut, ...(c.shortcut_alt || [])].filter(Boolean).some(match));
  if (command) { event.preventDefault(); execute(command); }
}
export async function initCommands(errorHandler) {
  onError = errorHandler;
  initMenus();
  try {
    const response = await fetch("/front/commands.json");
    if (!response.ok) throw new Error("commands.json: " + response.status);
    layouts = await response.json();
    installHandlers();
    const ids = { FILE: "file", EDIT: "edit", TEXT: "text", CONFIG: "configuration", DEBUGGER: "debug", ADMIN: "administration", SERVICE: "service", WINDOW: "window", HELP: "help" };
    for (const menu of layouts.menus) {
      addMenu("menu-" + ids[menu.key.replace("IDS_BAND_", "")], () => label(menu), () => menuEntries(menu));
    }
    const standard = layouts.toolbars.standard, config = layouts.toolbars.configuration, mod = layouts.toolbars.module_a;
    const modbar = toolbar(mod.items, label(mod)); modbar.id = "module-toolbar";
    fill($("toolbars"), el("div", { class: "toolbar-row" }, toolbar(standard.items, label(standard))),
      el("div", { class: "toolbar-row" }, toolbar(config.items, label(config)), modbar));
    const actions = el("button", { type: "button", "data-popup": "tree", title: T.actions, onclick: async (e) => {
      const rect = e.currentTarget.getBoundingClientRect();
      try { await loadTreeData(); showContext({ preventDefault() {}, clientX: rect.left, clientY: rect.bottom }, layouts.tree_contexts.actions.map(item)); refreshCommands(); } catch (error) { onError(error); }
    } }, T.actions + " ▾");
    fill($("tree-actions"), actions, ...layouts.tree_toolbar.filter((c) => c.type === "command").map(button));
    const palette = toolbar(layouts.palette, T.palette); palette.id = "palette-toolbar";
    $("palette-title").after(palette);
    $("tree").addEventListener("contextmenu", async (event) => {
      event.preventDefault();
      const row = event.target.closest(".row");
      selectTreeRow(row); if (row?.dataset.path) setPaletteTarget(row.dataset.path);
      try { showContext(event, await treeItems()); refreshCommands(); } catch (error) { onError(error); }
    });
    $("windows").addEventListener("contextmenu", (event) => {
      if (event.target.closest('.win[data-kind="module"] .cm-editor')) showContext(event, layouts.module_context.map(item), { disabledHover: true });
    });
    $("palette").addEventListener("contextmenu", (event) => showContext(event, layouts.palette_context.map(item)));
    onActive(() => { closeAllMenus(); refreshCommands(); });
    document.addEventListener("commands-changed", () => {
      refreshCommands();
      loadTreeData().then(refreshCommands).catch(onError);
    });
    document.addEventListener("keydown", shortcuts);
    refreshCommands();
  } catch (error) { onError(error); }
}
