import { $, el, fill } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { activeWindow, openWindow } from "./tabs.js";

const MIN_QUERY = 2;
let timer = null;
let seq = 0;
let abort = null;
let local = false;
let globalFocus = false;
function mode(value) {
  local = value;
  $("searchbox").classList.toggle("global-search", !value);
  if (value) {
    seq++;
    if (abort) abort.abort();
    abort = null;
    fill($("results"));
  }
}
let treeTarget = false;
const history = [];
function localSearch(back = false) {
  const search = $("search").value;
  if (!search) return;
  if (search && !history.includes(search)) {
    history.unshift(search);
    history.splice(20);
    fill($("search-history"), history.map((value) => el("option", { value })));
  }
  if (treeTarget || activeWindow()?.kind !== "module") {
    $("tree-filter").value = search;
    $("tree-filter").dispatchEvent(new Event("input"));
    return;
  }
  const ctl = activeWindow()?.ctl;
  ctl?.setSearch?.(search, $("opt-regex").checked);
  if (back) ctl?.findPrevious?.(); else ctl?.findNext?.();
}

function run() {
  if (local) {
    fill($("results"));
    if (!treeTarget) activeWindow()?.ctl?.setSearch?.($("search").value, $("opt-regex").checked);
    else localSearch();
    return;
  }
  const q = $("search").value;
  const ul = $("results");
  const mine = ++seq;
  if (abort) abort.abort();
  abort = null;
  fill(ul);
  if (q.length < MIN_QUERY) return;
  const ctl = typeof AbortController === "function" ? new AbortController() : null;
  abort = ctl;
  api(
    "find",
    { q, regex: $("opt-regex").checked ? "1" : null, names: $("opt-names").checked ? "1" : null },
    ctl ? ctl.signal : null,
  ).then(
    (d) => {
      if (mine !== seq) return;
      if (!d.hits.length) {
        ul.appendChild(el("li", { class: "note" }, T.nothing));
        return;
      }
      ul.appendChild(
        el("li", { class: "note" }, T.found + d.total + (d.total > d.hits.length ? T.shown + d.hits.length : "")),
      );
      for (const h of d.hits) {
        const open = () =>
          h.module
            ? openWindow({ kind: "module", path: h.path, module: h.module, line: h.line, icon: "module:" + h.module })
            : openWindow({ kind: "object", path: h.path });
        ul.appendChild(
          el(
            "li",
            { class: "hit", "data-path": h.path, "data-module": h.module, "data-line": h.line || null, onclick: open },
            el("span", { class: "rp" }, h.path + (h.module ? "." + h.module : "") + (h.line ? ":" + h.line : "")),
            el("span", { class: "rt" }, h.text.trim()),
          ),
        );
      }
    },
    (e) => {
      if (mine !== seq) return;
      ul.appendChild(el("li", { class: "note" }, T.error + e.message));
    },
  );
}

function schedule() {
  clearTimeout(timer);
  timer = setTimeout(run, 300);
}

export function initSearch() {
  $("search").setAttribute("list", "search-history");
  $("searchbox").appendChild(el("datalist", { id: "search-history" }));
  const attach = () => {
    const toolbar = document.querySelector("#toolbars .toolbar");
    if (toolbar) { toolbar.appendChild($("searchbox")); observer.disconnect(); }
  };
  const observer = new MutationObserver(attach);
  observer.observe($("toolbars"), { childList: true, subtree: true });
  attach();
  mode(true);
  document.addEventListener("click", (e) => {
    const key = e.target.closest("[data-key]")?.dataset.key;
    if (key === "IDS_CMD_FINDINTEXTS") { globalFocus = true; mode(false); }
    else if (key === "IDS_CMD_FIND") mode(true);
  }, true);
  $("search").addEventListener("focus", () => { mode(!globalFocus); globalFocus = false; });
  document.addEventListener("focusin", (e) => {
    if (e.target.closest("#side")) treeTarget = true;
    else if (e.target.closest(".mdi-window")) treeTarget = false;
  });
  document.addEventListener("keydown", (e) => {
    if (e.target.closest("dialog") || e.target.closest('.win:not([data-kind="module"]) .cm-editor')) return;
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "f") {
      e.preventDefault(); e.stopImmediatePropagation();
      globalFocus = e.shiftKey; mode(!e.shiftKey); $("search").focus(); $("search").select(); globalFocus = false;
    } else if (e.key === "F3") {
      e.preventDefault(); e.stopImmediatePropagation(); mode(true); localSearch(e.shiftKey);
    }
  }, true);
  $("search").addEventListener("input", schedule);
  $("opt-names").addEventListener("change", schedule);
  $("opt-regex").addEventListener("change", schedule);
  $("search").addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault(); if (local) localSearch(e.shiftKey); else run();
    } else if (e.key === "Escape") {
      $("search").value = "";
      run();
    }
  });
}
