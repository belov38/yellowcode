import { $, el, fill } from "./dom.js";
import { api, PORTION } from "./api.js";
import { T } from "./i18n.js";
import { icon } from "./icon.js";
import { openMetadata } from "./metadata_open.js";
import { setPaletteTarget } from "./props.js";

const cache = new Map();
let selRow = null;
export const selectedTreeRow = () => selRow;
export function selectTreeRow(row) {
  if (selRow) selRow.classList.remove("sel");
  selRow = row;
  if (row) row.classList.add("sel");
}

export function markTree(path) {
  if (selRow) {
    selRow.classList.remove("sel");
    selRow = null;
  }
  if (!path) return;
  for (const r of $("tree").querySelectorAll(".row[data-path]")) {
    if (r.getAttribute("data-path") === path) {
      selRow = r;
      r.classList.add("sel");
      break;
    }
  }
}

function loadChildren(id) {
  if (cache.has(id)) return Promise.resolve(cache.get(id));
  return api("tree", { id }).then((kids) => {
    cache.set(id, kids);
    return kids;
  });
}

function makeNode(n, onError) {
  const li = el("li", { "data-id": n.id });
  const tw = el("span", { class: "tw" }, n.children > 0 ? "⊕" : "");
  const row = el(
    "div",
    { class: "row", "data-id": n.id, "data-path": n.path, "data-module": n.module, "data-icon": n.icon },
    tw,
    icon(n.icon),
    el("span", { class: "lb" }, n.label),
    n.synonym ? el("span", { class: "sy" }, n.synonym) : null,
    n.type_text ? el("span", { class: "sy" }, n.type_text) : null,
  );
  li.appendChild(row);
  const state = { open: false, ul: null };
  li.setTreeChildren = (ul) => { state.ul = ul; state.open = true; li.appendChild(ul); tw.textContent = "⊟"; };
  li.openTree = () => state.open ? Promise.resolve() : toggle(li, n, tw, state, onError);
  tw.addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (n.children > 0) toggle(li, n, tw, state, onError);
  });
  row.addEventListener("click", () => {
    $("side").focus({ preventScroll: true });
    selectTreeRow(row);
    if (n.path) setPaletteTarget(n.path);
    document.dispatchEvent(new Event("commands-changed"));
  });
  row.activate = () => {
    if (n.path) {
      openMetadata(n);
    } else if (n.children > 0) toggle(li, n, tw, state, onError);
  };
  row.expandTree = (open) => {
    if (n.children > 0 && state.open !== open) toggle(li, n, tw, state, onError);
  };
  row.addEventListener("dblclick", (ev) => {
    if (ev.target === tw) return;
    window.getSelection()?.removeAllRanges();
    row.activate();
  });
  return li;
}

function toggle(li, n, tw, state, onError) {
  if (state.open) {
    state.open = false;
    tw.textContent = "⊕";
    if (state.ul) state.ul.hidden = true;
    return;
  }
  state.open = true;
  tw.textContent = "⊖";
  if (state.ul) {
    state.ul.hidden = false;
    return;
  }
  state.ul = el("ul");
  li.appendChild(state.ul);
  return loadChildren(n.id).then(
    (kids) => renderKids(state.ul, kids, 0, onError),
    (e) => {
      if (state.ul) state.ul.remove();
      state.open = false;
      state.ul = null;
      tw.textContent = "⊕";
      onError(e);
    },
  );
}

function renderKids(ul, kids, from, onError) {
  const end = Math.min(from + PORTION, kids.length);
  const frag = document.createDocumentFragment();
  for (let i = from; i < end; i++) frag.appendChild(makeNode(kids[i], onError));
  ul.appendChild(frag);
  if (end < kids.length) {
    const more = el("li", { class: "more" }, T.more + " (" + (kids.length - end) + ")");
    more.addEventListener("click", () => {
      more.remove();
      renderKids(ul, kids, end, onError);
    });
    ul.appendChild(more);
  }
  if (!selRow || !selRow.isConnected) markTree(selPath);
}

let selPath = null;

export function selectInTree(path) {
  selPath = path;
  markTree(path);
}

export async function revealInTree(path) {
  async function route(id, ancestors) {
    for (const node of await loadChildren(id)) {
      if (node.module) continue;
      const kind = path.split(".")[0];
      if (!node.path && node.icon?.startsWith("section:") && node.icon !== "section:" + kind &&
          !(node.icon === "section:Document" && ["DocumentNumerator", "Sequence"].includes(kind))) continue;
      if (node.path === path) return [...ancestors, node.id];
      if (node.children && (!node.path || path.startsWith(node.path + "."))) {
        const found = await route(node.id, [...ancestors, node.id]);
        if (found) return found;
      }
    }
    return null;
  }
  const ids = await route(0, []);
  if (!ids) return;
  let host = $("tree").querySelector(":scope > ul");
  for (const id of ids) {
    let node = [...host.children].find((li) => li.dataset.id === String(id));
    while (!node && host.querySelector(":scope > .more")) {
      host.querySelector(":scope > .more").click();
      node = [...host.children].find((li) => li.dataset.id === String(id));
    }
    if (!node) return;
    if (id !== ids[ids.length - 1]) { await node.openTree(); host = node.querySelector(":scope > ul"); }
    else {
      selectTreeRow(node.querySelector(":scope > .row"));
      selRow.scrollIntoView({ block: "nearest" });
    }
  }
}

export function initTree(onError) {
  let generation = 0, timer;
  let original = null;
  $("tree-filter").addEventListener("input", () => {
    const mine = ++generation, term = $("tree-filter").value.toLocaleLowerCase().replaceAll("ё", "е");
    clearTimeout(timer);
    if (!original) original = $("tree").firstElementChild;
    if (!term) { fill($("tree"), original); original = null; return; }
    timer = setTimeout(async () => {
      try {
        const result = await api("find", { q: term, names: 1, tree: 1 });
        const paths = new Set(result.hits.map((h) => h.path));
        const kinds = new Set([...paths].map((p) => p.split(".")[0]));
        const ancestors = new Set();
        for (const path of paths) {
          const parts = path.split(".");
          for (let end = 1; end <= parts.length; end++) ancestors.add(parts.slice(0, end).join("."));
        }
        const relevant = (n) => !n.path || ancestors.has(n.path);
        async function project(id) {
          const ul = el("ul");
          for (const n of await loadChildren(id)) {
            if (mine !== generation) return ul;
            if (n.module || !relevant(n)) continue;
            if (!n.path && n.icon?.startsWith("section:") && !kinds.has(n.icon.slice(8)) &&
                !(n.icon === "section:Document" && (kinds.has("DocumentNumerator") || kinds.has("Sequence")))) continue;
            const li = makeNode(n, onError);
            if (n.children) {
              const kids = await project(n.id);
              if (kids.children.length) {
                li.setTreeChildren(kids);
              }
            }
            if (!paths.has(n.path) && !li.querySelector("ul > li")) continue;
            for (const label of li.querySelectorAll(":scope > .row > .lb, :scope > .row > .sy")) {
              const text = label.textContent, lower = text.toLocaleLowerCase().replaceAll("ё", "е");
              fill(label);
              let from = 0, at;
              while ((at = lower.indexOf(term, from)) >= 0) {
                label.append(text.slice(from, at), el("span", { class: "tree-match" }, text.slice(at, at + term.length)));
                from = at + term.length;
              }
              label.append(text.slice(from));
            }
            ul.appendChild(li);
          }
          return ul;
        }
        const filtered = await project(0);
        if (mine === generation) fill($("tree"), filtered);
      } catch (error) { if (mine === generation) onError(error); }
    }, 150);
  });
  $("side").addEventListener("keydown", (ev) => {
    if (ev.altKey || ev.ctrlKey || ev.metaKey || ev.target.closest("input, button, select, textarea") || !selRow?.activate) return;
    if (ev.key === "Enter") selRow.activate();
    else if (ev.key === "ArrowRight" || ev.key === "ArrowLeft") selRow.expandTree(ev.key === "ArrowRight");
    else return;
    ev.preventDefault();
  });
  const root = el("ul");
  fill($("tree"), root);
  return loadChildren(0).then((kids) => renderKids(root, kids, 0, onError));
}
