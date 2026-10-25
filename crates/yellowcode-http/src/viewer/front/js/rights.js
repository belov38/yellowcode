import { el, fill, clear } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { icon } from "./icon.js";
import { registerKind } from "./tabs.js";

const ROWS = 300;

function rowIcon(object) {
  const head = object.split(".")[0];
  return head === "Configuration" ? "root" : "object:" + head;
}

function createRights(win) {
  const host = el("div", { class: "rightswin" }, el("div", { class: "msg" }, T.loading));
  win.pane.appendChild(host);
  let pop = null;
  const closePop = () => {
    if (pop) pop.remove();
    pop = null;
  };
  const onDoc = (ev) => { if (pop && !pop.contains(ev.target) && !ev.target.closest(".rls")) closePop(); };
  const onKey = (ev) => { if (ev.key === "Escape") closePop(); };
  document.addEventListener("click", onDoc);
  document.addEventListener("keydown", onKey);

  function showPop(btn, title, conditions) {
    closePop();
    pop = el("div", { class: "rls-pop", role: "dialog", "aria-label": T.rlsTitle },
      el("div", { class: "rls-head" }, T.rlsTitle + ": " + title),
      conditions.map((c) => el("pre", { class: "rls-text" }, c)));
    win.pane.appendChild(pop);
    const b = btn.getBoundingClientRect();
    const p = win.pane.getBoundingClientRect();
    pop.style.left = Math.max(0, Math.min(b.left - p.left + win.pane.scrollLeft, p.width - 420)) + "px";
    pop.style.top = b.bottom - p.top + win.pane.scrollTop + 4 + "px";
  }

  function view(d) {
    if (!d.defined) {
      return el("div", { class: "msg", role: "note", "data-defined": "false" }, T.rightsUndefined);
    }
    const cols = d.columns || [];
    const titles = d.titles || [];
    const hints = d.hints || [];
    const flags = d.flags || {};
    const flagBar = el("div", { class: "rflags" },
      [["set_for_new_objects", T.flagNew], ["set_for_attributes_by_default", T.flagAttrs],
        ["independent_rights_of_child_objects", T.flagIndep]].map(([k, label]) =>
        el("label", { class: "rflag", "data-flag": k }, el("input", { type: "checkbox", disabled: true, checked: !!flags[k] }), " " + label)));
    const filter = el("input", { type: "search", class: "rfilter", placeholder: T.rightsFilter, "aria-label": T.rightsFilter, autocomplete: "off" });
    const granted = el("input", { type: "checkbox", class: "rgranted" });
    const count = el("span", { class: "msg rcount" });
    const bar = el("div", { class: "fbar" }, filter, el("label", null, granted, " " + T.onlyGranted), count);

    const head = el("tr", null, el("th", { class: "robj" }, T.objectCol),
      cols.map((c, i) => el("th", { class: "rcol", "data-right": c, title: hints[i] ? titles[i] + "\n" + hints[i] : titles[i] },
        el("span", null, titles[i] || c))));
    const body = el("tbody");
    const foot = el("div", { class: "fmore" });

    const makeRow = (r) => {
      const rls = new Map();
      for (const x of r.restricted || []) rls.set(x.right, x.conditions || []);
      const tr = el("tr", { "data-object": r.object },
        el("th", { class: "robj", title: r.kind + " " + r.object }, icon(rowIcon(r.object)), el("span", { class: "oname" }, r.object)));
      r.values.forEach((v, i) => {
        const conds = rls.get(cols[i]);
        const td = el("td", { class: v === null ? "na" : v ? "on" : "off", "data-right": cols[i] });
        if (conds) {
          td.classList.add("restricted");
          const b = el("button", { class: "rls", type: "button", "aria-label": T.rlsTitle + ": " + (titles[i] || cols[i]) },
            (v ? "✓" : "") + "*");
          b.addEventListener("click", () => showPop(b, (titles[i] || cols[i]) + " — " + r.object, conds));
          td.appendChild(b);
        } else {
          td.textContent = v === null ? "—" : v ? "✓" : "";
        }
        tr.appendChild(td);
      });
      return tr;
    };

    let loadSeq = 0;
    let timer = null;
    const loadRows = (from, first, moreBtn) => {
      const mine = ++loadSeq;
      closePop();
      return api("rights", {
        path: win.path, q: filter.value.trim() || null, only: granted.checked ? "1" : null, from, limit: ROWS,
      }).then(
        (r) => {
          if (mine !== loadSeq || win.closed) return;
          if (first) clear(body);
          const frag = document.createDocumentFragment();
          for (const row of r.rows) frag.appendChild(makeRow(row));
          body.appendChild(frag);
          const have = r.from + r.rows.length;
          count.textContent = T.rowsN + r.total;
          fill(foot);
          if (have < r.total && r.rows.length) {
            foot.appendChild(el("button", {
              class: "btn more-btn", type: "button", onclick: (ev) => { ev.target.disabled = true; loadRows(have, false, ev.target); },
            }, T.moreRows + (r.total - have)));
          }
        },
        (e) => {
          if (moreBtn) moreBtn.disabled = false;
          if (mine === loadSeq && !win.closed) count.textContent = T.error + e.message;
        },
      );
    };
    const showFirst = (r) => {
      const frag = document.createDocumentFragment();
      for (const row of r.rows) frag.appendChild(makeRow(row));
      body.appendChild(frag);
      count.textContent = T.rowsN + r.total;
      if (r.rows.length < r.total) {
        foot.appendChild(el("button", {
          class: "btn more-btn", type: "button", onclick: (ev) => { ev.target.disabled = true; loadRows(r.rows.length, false, ev.target); },
        }, T.moreRows + (r.total - r.rows.length)));
      }
    };
    showFirst(d);
    const refresh = () => {
      clearTimeout(timer);
      timer = setTimeout(() => loadRows(0, true), 200);
    };
    filter.addEventListener("input", refresh);
    granted.addEventListener("change", () => {
      clearTimeout(timer);
      loadRows(0, true);
    });

    const root = el("div", { class: "rightsview" }, flagBar, bar,
      el("div", { class: "rwrap" }, el("table", { class: "rtable", "aria-label": T.rightsTable }, el("thead", null, head), body)), foot);
    const tpl = d.templates || [];
    if (tpl.length) {
      const sec = el("details", { class: "dsec rtpl" }, el("summary", null, T.templatesH2 + " (" + tpl.length + ")"));
      sec.addEventListener("toggle", () => {
        if (!sec.open || sec.querySelector(".rtpl-item")) return;
        for (const [name, text] of tpl) sec.appendChild(el("div", { class: "rtpl-item" }, el("strong", null, name), el("pre", { class: "rls-text" }, text)));
      });
      root.appendChild(sec);
    }
    return root;
  }

  api("rights", { path: win.path, templates: "1", limit: ROWS }).then(
    (d) => { if (!win.closed) fill(host, el("div", { class: "wtitle" }, el("span", { class: "kind" }, T.rightsTable), el("span", { class: "name" }, win.title)), view(d)); },
    (e) => { if (!win.closed) fill(host, el("div", { class: "msg bad" }, T.error + e.message)); },
  );
  return {
    destroy() {
      closePop();
      document.removeEventListener("click", onDoc);
      document.removeEventListener("keydown", onKey);
    },
  };
}

export function initRights() {
  registerKind("rights", { create: createRights });
}
