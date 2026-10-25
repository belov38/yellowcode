import { $, el, fill } from "./dom.js";
import { api, show } from "./api.js";
import { T, modLabel } from "./i18n.js";
import { icon, kindIconKey } from "./icon.js";
import { isTemplate, openMetadata } from "./metadata_open.js";
export { isTemplate } from "./metadata_open.js";
import "./predefined.js";
import { metadataWidgets } from "./metadata_widgets.js";
import { metadataTab } from "./metadata_tabs.js";
import { genFormButton } from "./genform.js";
import { closeWindow, openWindow, registerKind, tabStrip, update } from "./tabs.js";

let seq = 0;
let alpha = false;
let lastShow = null;

export function isForm(path) {
  return /\.(Form|Форма)\./.test(path || "") || /^(CommonForm|ОбщаяФорма)\./.test(path || "");
}

export function isRole(path) {
  return /^(Role|Роль)\.[^.]+$/.test(path || "");
}

function viewButtons(path) {
  const make = (cls, text, spec) =>
    el("button", { class: "btn " + cls, type: "button", onclick: () => openWindow(spec) }, text);
  return [
    isForm(path) ? make("open-form", T.openForm, { kind: "form", path, icon: "sub:Form" }) : null,
    isTemplate(path) ? make("open-template", T.openTemplate, { kind: "template", path, icon: "sub:Template" }) : null,
    isRole(path) ? make("open-rights", T.openRights, { kind: "rights", path, icon: "object:Role" }) : null,
  ];
}

function valueView(p) {
  const text = p.text === undefined || p.text === null ? "" : String(p.text);
  if (p.view === "bool") {
    return el("input", { type: "checkbox", class: "vbool", disabled: true, checked: p.value === true, "aria-label": p.title });
  }
  const field = el("span", { class: "vtext", title: text }, text);
  if (p.view === "enum" || p.view === "md-ref") {
    return el("span", { class: "vfield vcombo", "data-view": p.view }, field, el("span", { class: "varrow", "aria-hidden": "true" }, "▾"));
  }
  if (p.view === "type" || p.view === "list" || p.view === "button") {
    return el("span", { class: "vfield vdots", "data-view": p.view }, field, el("span", { class: "vbtn", "aria-hidden": "true" }, "…"));
  }
  return el("span", { class: "vfield", "data-view": p.view || "text" }, field);
}

function propRow(p) {
  return el(
    "tr",
    { "data-name": p.name, "data-view": p.view || null },
    el("td", { class: "k", title: p.tooltip || p.name }, p.title),
    el("td", { class: "v" }, valueView(p)),
  );
}

function propTable(props) {
  return el("table", { class: "props", "aria-label": T.prop + " / " + T.value }, el("tbody", null, props.map(propRow)));
}

function group(title, content) {
  return el("details", { class: "pgroup", open: "" }, el("summary", null, title), content);
}

function renderPalette(d) {
  const body = $("palette-body");
  const props = d.properties || [];
  if (alpha) {
    const sorted = props.slice().sort((a, b) => a.title.localeCompare(b.title));
    fill(body, propTable(sorted));
    return;
  }
  const groups = new Map();
  for (const p of props) {
    const g = p.group || T.otherGroup;
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(p);
  }
  const rank = (g) => {
    const i = (d.groups || []).indexOf(g);
    return g === T.otherGroup ? 1e9 : i < 0 ? 1e8 : i;
  };
  const names = [...groups.keys()].sort((a, b) => rank(a) - rank(b));
  fill(body, names.map((g) => group(g, propTable(groups.get(g)))));
}

export function setPaletteTarget(path) {
  const mine = ++seq;
  const body = $("palette-body");
  const title = $("palette-title");
  lastShow = null;
  if (!path) {
    fill(title);
    fill(body, el("div", { class: "msg" }, T.pick));
    return;
  }
  fill(body, el("div", { class: "msg" }, T.loading));
  show(path).then(
    (d) => {
      if (mine !== seq) return;
      lastShow = d;
      fill(title, el("span", { class: "kind" }, d.kind), el("span", { class: "name" }, d.name));
      renderPalette(d);
    },
    (e) => {
      if (mine !== seq) return;
      fill(title);
      fill(body, el("div", { class: "msg bad" }, T.error + e.message));
    },
  );
}

function listNode(list) {
  const host = el("div", { class: "olist", "data-kind": list.kind });
  host.appendChild(el("h3", null, list.title || list.kind));
  const items = list.items || [];
  if (!items.length) {
    host.appendChild(el("div", { class: "msg" }, T.none));
    return host;
  }
  const ul = el("ul", { class: "links" });
  for (const it of items) {
    const act = () => openMetadata(it);
    ul.appendChild(
      el(
        "li",
        null,
        icon(it.icon),
        el("a", {
          tabindex: "0", "data-path": it.path, onclick: act,
          onkeydown: (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); act(); } },
        }, it.name),
        it.synonym ? el("span", { class: "syn" }, " " + it.synonym) : null,
      ),
    );
  }
  host.appendChild(ul);
  if (list.truncated) host.appendChild(el("div", { class: "msg" }, T.truncated + items.length + " / " + list.total));
  return host;
}

function captionNode(it) {
  const row = el("div", { class: "ocap", "data-control": it.control || null }, el("span", { class: "olabel" }, it.caption));
  if (it.open) {
    const o = it.open;
    row.appendChild(
      el("button", {
        class: "btn open-module", type: "button",
        onclick: () => openWindow({ kind: "module", path: o.path, module: o.module, icon: "module:" + o.module }),
      }, T.openBtn),
    );
  }
  return row;
}

function itemNode(it) {
  if (it.list) return listNode(it.list);
  if (it.caption !== undefined) return captionNode(it);
  return el(
    "div",
    { class: "orow", "data-name": it.name, "data-view": it.view || null },
    el("span", { class: "olabel", title: it.tooltip || it.name }, it.title),
    valueView(it),
  );
}


function tabNode(tab, path, allTabs) {
  const items = tab.items || [];
  if (items.some((it) => it.name === "Name")) {
    const fields = new Map(allTabs.flatMap((t) => t.items || []).filter((it) => it.name).map((it) => [it.name, it]));
    const widgets = metadataWidgets((name) => fields.get(name));
    return el("div", { class: "oitems object-basics" }, items.map((it) => {
      if (it.control === "stTypeDomain") return widgets.field("Type", it.caption);
      if (it.name === "CharacteristicExtValues") return widgets.field(it.name);
      if (!it.name || !["text", "number"].includes(it.view)) return itemNode(it);
      const explanation = it.name === "Explanation";
      const field = el(explanation ? "textarea" : "input", {
        class: "object-field", readonly: "", value: it.text || "", "aria-label": it.title,
        title: it.tooltip || it.title, spellcheck: "false", tabindex: "0",
      });
      return el("label", { class: "orow" + (/Presentation$/.test(it.name) ? " object-presentation" : ""), "data-name": it.name },
        el("span", { class: "olabel" }, it.title + ":"), field);
    }));
  }
  if (/^ChartOfAccounts\.[^.]+$/.test(path || "") && ["Прочее", "Other"].includes(tab.title)) return metadataTab(tab, path, allTabs, itemNode);
  if (/^(Document|Catalog|DocumentJournal|Enum|ChartOfCharacteristicTypes|InformationRegister)\.[^.]+$/.test(path || "")) return metadataTab(tab, path, allTabs, itemNode);
  if (!items.length) return el("div", { class: "msg" }, T.emptyTab);
  return el("div", { class: "oitems" }, items.map(itemNode));
}

function modulesBar(d, win) {
  const mods = d.modules || [];
  if (!mods.length) return null;
  const bar = el("div", { class: "list-modules wmods" }, el("span", { class: "dim" }, T.modulesInline));
  for (const m of mods) {
    const act = () => openWindow({ kind: "module", path: win.path, module: m, icon: "module:" + m });
    bar.appendChild(
      el("a", {
        tabindex: "0", "data-path": win.path, "data-module": m, onclick: act,
        onkeydown: (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); act(); } },
      }, modLabel(m)),
    );
  }
  return bar;
}

function createObject(win) {
  const title = el("div", { class: "wtitle" });
  const host = el("div", { class: "owin" });
  win.pane.append(title, host);
  fill(host, el("div", { class: "msg" }, T.loading));
  api("object", { path: win.path }).then(
    (d) => {
      if (win.closed) return;
      fill(title, el("span", { class: "kind" }, d.kind), el("span", { class: "name" }, d.name));
      const en = document.documentElement.lang === "en";
      win.frame.classList.add("object-editor");
      win.frame.dataset.objectKind = win.path.split(".")[0];
      update(win, { title: d.kind + " " + d.name });
      win.caption.prepend(icon(win.iconKey || kindIconKey(win.path.split(".")[0])), " ");
      title.hidden = true;
      const sections = (d.tabs || []).filter((t) => !(/^(Document|Catalog|DocumentJournal|Enum|ChartOfCharacteristicTypes|ChartOfAccounts|InformationRegister)\.[^.]+$/.test(win.path) && ["Интерфейсы", "Interfaces"].includes(t.title) && (t.items || []).every((it) => it.caption !== undefined)));
      const sectionNodes = sections.map((t) => tabNode(t, win.path, sections));
      const back = el("button", { class: "btn object-back", type: "button", onclick: () => strip.select(strip.index() - 1) }, en ? "< Back" : "<Назад");
      const next = el("button", { class: "btn object-next", type: "button", onclick: () => strip.select(strip.index() + 1) }, en ? "Next >" : "Далее>");
      const strip = tabStrip(
        sections.map((t, i) => ({ title: t.title, node: sectionNodes[i] })), T.windowTabs, 0,
        (i) => { back.disabled = i === 0; next.disabled = i === sections.length - 1; sectionNodes[i].dispatchEvent(new Event("object-tab-open")); },
      );
      const tabbar = strip.root.querySelector(".otabs");
      tabbar.setAttribute("aria-orientation", "vertical");
      const actions = el("details", { class: "object-actions" },
        el("summary", { class: "btn" }, en ? "Actions ▾" : "Действия ▾"),
        el("div", { class: "object-actions-popup" }, viewButtons(win.path), genFormButton(win.path), modulesBar(d, win),
          el("button", { class: "btn", type: "button", onclick: () => { document.body.classList.remove("no-palette"); actions.open = false; } }, T.palShow)));
      fill(host, strip.root,
        el("div", { class: "object-bottom" }, actions, back, next,
          el("button", { class: "btn object-close", type: "button", onclick: () => closeWindow(win) }, en ? "Close" : "Закрыть"),
          el("button", { class: "btn", type: "button", onclick: () => openWindow({ kind: "helper", path: "Helper", title: en ? "Help" : "Справка" }) }, en ? "Help" : "Справка")));

    },
    (e) => {
      if (win.closed) return;
      fill(host, el("div", { class: "msg bad" }, T.error + e.message));
    },
  );
  return {};
}

export function initProps() {
  registerKind("object", { create: createObject });
  const sw = $("opt-alpha");
  sw.addEventListener("change", () => {
    alpha = sw.checked;
    if (lastShow) renderPalette(lastShow);
  });
  setPaletteTarget(null);
}
