import { el, fill } from "./dom.js";
import { api } from "./api.js";
import { registerKind } from "./tabs.js";
import { metadataWidgets } from "./metadata_widgets.js";

function create(win) {
  const en = document.documentElement.lang === "en";
  const tr = (ru, english) => en ? english : ru;
  win.frame.classList.add("metadata-predefined-window");
  const body = el("div", { class: "metadata-predefined-body", "aria-busy": "true" });
  const toolbar = metadataWidgets(() => null).toolbar();
  toolbar.prepend(el("button", { type: "button", disabled: true, class: "metadata-predefined-actions" }, tr("Действия", "Actions") + " ▾"));
  fill(win.pane, toolbar, body);
  api("predefined", { path: win.path }).then((data) => {
    if (win.closed) return;
    if (data.available === false) { fill(body, el("div", { class: "msg" }, tr("Предопределенные данные недоступны", "Predefined data unavailable"))); return; }
    const flags = (data.flags || []).map((flag) => typeof flag === "string" ? { name: flag, title: flag } : flag);
    const identify = (items, parent = "") => items.map((item) => ({ ...item, id: item.id || parent + "." + item.name, children: identify(item.children || [], parent + "." + item.name) }));
    let items = identify(data.items || []);
    const accounts = data.kind === "ChartOfAccounts";
    if (accounts && items.length) items = [{ id: "root:" + win.path, name: tr("Счета", "Accounts"), code: "", description: "", account: null, children: items }];
    const headers = [tr("Имя", "Name"), tr("Код", "Code"), tr("Наименование", "Description")];
    if (accounts) headers.push(tr("Вид", "Type"), tr("Забалансовый", "Off-balance"), tr("Порядок", "Order"), ...flags.map((flag) => flag.title || flag.name));
    const tbody = el("tbody");
    const table = el("table", { class: "metadata-predefined-table" + (accounts ? " metadata-accounts-table" : ""), role: "treegrid", "aria-label": tr("Предопределенные элементы", "Predefined items") },
      el("colgroup", null, headers.map((_, index) => el("col", { style: "width:" + ([150, 85, 175, 80, 75, 75][index] || 85) + "px" }))),
      el("thead", null, el("tr", null, headers.map((title) => el("th", null, title)))), tbody);
    const expanded = new Set();
    let selected = items[0]?.id;
    const checkbox = (checked, label) => el("input", { type: "checkbox", checked, disabled: true, "aria-label": label });
    const render = (focus = false) => {
      const rows = [];
      const visit = (items, depth, parent) => {
        for (const item of items) {
          const children = item.children || [];
          const open = expanded.has(item.id);
          const node = el("tr", { class: selected === item.id ? "cur" : null, tabindex: selected === item.id ? "0" : "-1", "data-id": item.id,
            "aria-level": depth + 1, "aria-selected": String(selected === item.id), "aria-expanded": children.length ? String(open) : null,
            onclick: () => { selected = item.id; render(true); },
            onkeydown: (ev) => {
              const visible = [...tbody.children]; const index = visible.indexOf(node);
              let target;
              if (ev.key === "ArrowDown") target = visible[Math.min(index + 1, visible.length - 1)];
              else if (ev.key === "ArrowUp") target = visible[Math.max(index - 1, 0)];
              else if (ev.key === "ArrowRight" && children.length) { if (open) selected = children[0].id; else expanded.add(item.id); ev.preventDefault(); render(true); return; }
              else if (ev.key === "ArrowLeft") {
                if (open) expanded.delete(item.id); else if (parent) selected = parent.id;
                ev.preventDefault(); render(true); return;
              } else return;
              ev.preventDefault(); if (target) { selected = target.dataset.id; render(true); }
            } });
          const toggle = children.length ? el("button", { type: "button", tabindex: "-1", "aria-label": tr(open ? "Свернуть " : "Развернуть ", open ? "Collapse " : "Expand ") + item.name,
            onclick: (ev) => { ev.stopPropagation(); selected = item.id; if (open) expanded.delete(item.id); else expanded.add(item.id); render(true); } }, open ? "−" : "+") : el("span", { class: "metadata-predefined-indent" });
          node.append(el("td", { style: "padding-left:" + (3 + depth * 14) + "px", title: item.name }, toggle, item.name), el("td", { title: item.code }, item.code), el("td", { title: item.description }, item.description));
          if (accounts) {
            const account = item.account;
            const types = { Active: tr("Активный", "Active"), Passive: tr("Пассивный", "Passive"), ActivePassive: tr("Активно-пассивный", "Active/passive") };
            node.append(el("td", null, types[account?.type] || ""), el("td", null, account ? checkbox(account.off_balance, headers[4]) : null), el("td", null, account?.order));
            flags.forEach((flag, index) => {
              const stored = account?.flags?.[index];
              const value = typeof stored === "boolean" || stored == null ? stored : account.flags.find((value) => value.name === flag.name)?.value;
              node.append(el("td", null, value == null ? null : checkbox(value, flag.title || flag.name)));
            });
          }
          rows.push(node);
          if (open) visit(children, depth + 1, item);
        }
      };
      visit(items, 0, null); fill(tbody, rows);
      if (focus) tbody.querySelector(".cur")?.focus({ preventScroll: true });
      if (focus) tbody.querySelector(".cur")?.scrollIntoView({ block: "nearest", inline: "nearest" });
    };
    const expandAll = (items) => { for (const item of items) { if (item.children?.length) { expanded.add(item.id); expandAll(item.children); } } };
    expandAll(items); render();
    fill(body, items.length ? table : el("div", { class: "msg" }, tr("Нет предопределенных элементов", "No predefined items")),
      data.truncated ? el("div", { class: "msg" }, tr("Показана часть предопределенных элементов", "Some predefined items are omitted")) : null);
  }).catch((error) => { if (!win.closed) fill(body, el("div", { class: "msg bad" }, error.message)); })
    .finally(() => body.removeAttribute("aria-busy"));
}

registerKind("predefined", { create });
