import { el, fill, portions } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { registerKind, tabStrip } from "./tabs.js";

const SIZES = [[1024, 768], [1440, 900], [1920, 1080]];
const ROWS = 500;
const EXPAND_ALL_MAX = 5000;
let formSize = 1;

function countItems(items) {
  let n = 0;
  const stack = items.slice();
  while (stack.length) {
    const it = stack.pop();
    n++;
    for (const c of it.children || []) stack.push(c);
  }
  return n;
}

function itemNode(it) {
  const kids = it.children || [];
  const li = el("li", { class: "fnode", "data-id": it.id, "data-tag": it.tag });
  const tw = el("span", { class: "tw", role: kids.length ? "button" : null, tabindex: kids.length ? "0" : null,
    "aria-expanded": kids.length ? "false" : null }, kids.length ? "▸" : "");
  li.appendChild(el("div", { class: "frow" }, tw, el("span", { class: "fname" }, it.name || "—"),
    el("span", { class: "ftag" }, it.tag), el("span", { class: "fid dim" }, String(it.id))));
  let ul = null;
  let more = null;
  const expand = (deep) => {
    if (!kids.length) return;
    if (!ul) {
      ul = el("ul");
      more = el("div", { class: "fmore" });
      li.append(ul, more);
      portions(ul, more, kids, itemNode, ROWS, (r) => T.moreRows + r);
    }
    ul.hidden = false;
    tw.textContent = "▾";
    tw.setAttribute("aria-expanded", "true");
    if (deep) for (const c of ul.children) if (c.expand) c.expand(deep);
  };
  const collapse = () => {
    if (ul) ul.hidden = true;
    tw.textContent = "▸";
    tw.setAttribute("aria-expanded", "false");
  };
  const toggle = () => (ul && !ul.hidden ? collapse() : expand(false));
  if (kids.length) {
    tw.addEventListener("click", toggle);
    tw.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
  }
  li.expand = expand;
  return li;
}

function elementsPanel(d) {
  const items = d.items || [];
  const total = countItems(items);
  const root = el("ul", { class: "ftree", role: "tree" });
  const foot = el("div", { class: "fmore" });
  portions(root, foot, items, itemNode, ROWS, (r) => T.moreRows + r);
  const all = el("button", {
    class: "btn expand-items", type: "button",
    onclick: () => { for (const c of root.children) if (c.expand) c.expand(total <= EXPAND_ALL_MAX ? "all" : false); },
  }, T.expandAllItems);
  const bar = el("div", { class: "fbar" }, el("span", { class: "fcount" }, T.itemsN + total), all,
    d.truncated ? el("span", { class: "msg" }, T.truncated + total) : null);
  for (const c of root.children) if (c.expand) c.expand(false);
  return el("div", { class: "fpanel", "data-total": total }, bar, items.length ? root : el("div", { class: "msg" }, T.none), foot);
}

function listPanel(rows, cols, label) {
  if (!rows.length) return el("div", { class: "msg" }, T.none);
  const body = el("tbody");
  const foot = el("div", { class: "fmore" });
  portions(body, foot, rows, (r) => el("tr", null, cols.map((c) => el("td", null, c.cell(r)))), ROWS, (n) => T.moreRows + n);
  return el("div", { class: "fpanel" },
    el("table", { class: "ftable", "aria-label": label },
      el("thead", null, el("tr", null, cols.map((c) => el("th", null, c.title)))), body),
    foot);
}

const mark = (v) => (v ? "✓" : "");

function structureTabs(d) {
  return [
    { title: T.formElements, node: elementsPanel(d) },
    {
      title: T.formAttributes,
      node: listPanel(d.attributes || [], [
        { title: T.name, cell: (a) => a.name }, { title: T.idCol, cell: (a) => String(a.id) },
        { title: T.mainCol, cell: (a) => mark(a.main) },
      ], T.formAttributes),
    },
    {
      title: T.formCommands,
      node: listPanel(d.commands || [], [
        { title: T.name, cell: (c) => c.name }, { title: T.idCol, cell: (c) => String(c.id) },
        { title: T.actionCol, cell: (c) => c.action },
      ], T.formCommands),
    },
    {
      title: T.formParams,
      node: listPanel(d.parameters || [], [
        { title: T.name, cell: (p) => p.name }, { title: T.keyCol, cell: (p) => mark(p.key) },
      ], T.formParams),
    },
  ];
}

function previewPanel(win) {
  let seq = 0;
  const sel = el("select", { class: "form-size", "aria-label": "size" });
  SIZES.forEach((s, i) => sel.appendChild(el("option", { value: String(i) }, s[0] + "×" + s[1])));
  sel.value = String(formSize);
  const info = el("span", { class: "form-info msg" });
  const host = el("div", { class: "form-host" });
  const node = el("div", { class: "fpreview" }, el("div", { class: "formbar" }, sel, info), host);

  function load() {
    const mine = ++seq;
    const [w, h] = SIZES[formSize];
    info.textContent = "";
    info.className = "form-info msg";
    const fr = el("iframe", { sandbox: "allow-same-origin", title: win.title });
    fr.src = "/form?path=" + encodeURIComponent(win.path) + "&w=" + w + "&h=" + h;
    fr.style.width = w + "px";
    fr.style.height = h - 189 + 36 + "px";
    fill(host, fr);
    api("form", { path: win.path, w, h }).then(
      (d) => {
        if (mine !== seq || win.closed) return;
        let stubs = 0, skipped = 0;
        for (const k of Object.keys(d.stubs || {})) stubs += d.stubs[k];
        for (const k of Object.keys(d.skipped || {})) skipped += d.skipped[k];
        info.textContent = T.formStats
          .replace("%b", d.boxes.length).replace("%s", stubs).replace("%k", skipped).replace("%m", d.ms);
      },
      (e) => {
        if (mine !== seq || win.closed) return;
        info.textContent = T.error + e.message;
        info.className = "form-info msg bad";
      },
    );
  }
  sel.addEventListener("change", () => {
    formSize = parseInt(sel.value, 10) || 0;
    load();
  });
  return { node, load };
}

function createForm(win) {
  const preview = previewPanel(win);
  const holder = el("div", { class: "form-win" }, el("div", { class: "msg" }, T.loading));
  win.pane.appendChild(holder);
  let loaded = false;
  const ensurePreview = (i, n) => {
    if (i === n - 1 && !loaded) {
      loaded = true;
      preview.load();
    }
  };
  const build = (tabs) => {
    const all = tabs.concat([{ title: T.formPreview, node: preview.node }]);
    const strip = tabStrip(all, T.formTabs, 0, (i) => ensurePreview(i, all.length));
    fill(holder, strip.root);
  };
  api("form-structure", { path: win.path }).then(
    (d) => { if (!win.closed) build(structureTabs(d)); },
    (e) => {
      if (win.closed) return;
      build([{ title: T.formElements, node: el("div", { class: "msg bad" }, T.error + e.message) }]);
    },
  );
  return {};
}

export function initForm() {
  registerKind("form", { create: createForm });
}
