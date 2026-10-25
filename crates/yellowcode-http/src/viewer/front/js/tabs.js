import { $, el, clear } from "./dom.js";
import { T } from "./i18n.js";
import { showContext } from "./menu.js";
import { icon } from "./icon.js";
import { attachFloatingWindowGeometry } from "./window_geometry.js";

const MAX_WINDOWS = 16;
const kinds = new Map();
const wins = [];
const listeners = [];
let active = null;
let nextId = 1;
let topWindow = 0;
let tabTooltip = null, tooltipTimer = 0;
function hideTabTooltip() {
  clearTimeout(tooltipTimer);
  tabTooltip?.remove();
  tabTooltip = null;
}

export function registerKind(kind, impl) {
  kinds.set(kind, impl);
}

export function onActive(fn) {
  listeners.push(fn);
}

export function activeWindow() {
  return active;
}

function keyOf(kind, path, module) {
  return [kind, path, module || ""].join("\u0000");
}

export function hashOf(win) {
  if (!win) return "";
  const p = new URLSearchParams();
  p.set("path", win.path);
  if (win.kind === "module") p.set("module", win.module || "");
  else if (win.kind !== "object") p.set("kind", win.kind);
  return p.toString();
}

export function parseHash() {
  const p = new URLSearchParams(location.hash.replace(/^#/, ""));
  const path = p.get("path");
  if (!path) return null;
  const line = parseInt(p.get("line"), 10);
  const module = p.get("module");
  const kind = module !== null ? "module" : p.get("kind") || "object";
  return { kind, path, module, line: isNaN(line) ? null : line };
}

function writeHash() {
  const h = hashOf(active);
  if (!h) {
    if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  } else if (location.hash !== "#" + h) {
    location.hash = h;
  }
}

export function openWindow(spec, fromHash) {
  const impl = kinds.get(spec.kind);
  if (!impl || !spec.path) return null;
  const key = keyOf(spec.kind, spec.path, spec.module);
  let win = wins.find((w) => w.key === key);
  if (win) {
    if (spec.icon && !win.iconKey) setIcon(win, spec.icon);
    activate(win, fromHash);
    if (spec.line) {
      win.line = spec.line;
      if (win.ctl && win.ctl.goTo) win.ctl.goTo(spec.line);
    }
    return win;
  }
  if (wins.length >= MAX_WINDOWS) closeWindow(wins.find((w) => w !== active) || wins[0]);
  win = {
    id: nextId++, kind: spec.kind, path: spec.path, module: spec.module || null, line: spec.line || null,
    key, title: spec.title || spec.path.split(".").pop(), iconKey: null, closed: false, ctl: null,
  };
  win.pane = el("div", { class: "win", "data-kind": win.kind, "data-path": win.path, hidden: true });
  win.caption = el("span", { class: "mdi-title" }, win.title);
  win.frame = el("section", { class: "mdi-window", "data-window": win.id, "data-kind": win.kind },
    el("div", { class: "mdi-caption", ondblclick: () => maximize(win) }, win.caption,
      el("div", { class: "mdi-buttons" },
        el("button", { type: "button", "aria-label": "Minimize", onclick: () => minimize(win) }, el("span", { class: "chrome-glyph chrome-minimize", "aria-hidden": "true" })),
        el("button", { type: "button", "aria-label": "Maximize or restore", onclick: () => maximize(win) }, el("span", { class: "chrome-glyph chrome-maximize", "aria-hidden": "true" })),
        el("button", { type: "button", "aria-label": T.close, onclick: () => closeWindow(win) }, el("span", { class: "chrome-glyph chrome-close", "aria-hidden": "true" })))), win.pane);
  const offset = (wins.length % 6) * 24;
  win.frame.style.left = offset + "px";
  win.frame.style.top = offset + "px";
  win.frame.addEventListener("pointerdown", () => { if (active !== win) activate(win); });
  const caption = win.frame.querySelector(".mdi-caption");
  caption.addEventListener("pointerdown", (event) => {
    if (win.geometry || event.target.closest("button") || win.maximized) return;
    const x = event.clientX, y = event.clientY, left = win.frame.offsetLeft, top = win.frame.offsetTop;
    event.preventDefault();
    caption.setPointerCapture(event.pointerId);
    const move = (e) => {
      win.frame.style.left = Math.max(0, Math.min(left + e.clientX - x, document.getElementById("windows").clientWidth - 100)) + "px";
      win.frame.style.top = Math.max(0, Math.min(top + e.clientY - y, document.getElementById("windows").clientHeight - 24)) + "px";
    };
    caption.addEventListener("pointermove", move);
    caption.addEventListener("lostpointercapture", () => caption.removeEventListener("pointermove", move), { once: true });
  });
  win.label = el("span", { class: "tt" }, win.title);
  win.iconHost = el("span", { class: "tabicon" });
  win.tab = el(
    "div",
    { class: "tab", role: "tab", "aria-selected": "false", tabindex: "0", "data-kind": win.kind, "aria-label": win.title },
    win.iconHost,
    win.label,
    el("button", {
      class: "x", type: "button", "aria-label": T.close, title: T.close,
      onclick: (ev) => { ev.stopPropagation(); closeWindow(win); },
    }, "×"),
  );
  win.tab.addEventListener("mouseenter", () => {
    hideTabTooltip();
    tooltipTimer = setTimeout(() => {
      tabTooltip = el("div", { class: "mdi-tab-tooltip", role: "tooltip" }, win.title);
      document.body.appendChild(tabTooltip);
      const rect = win.tab.getBoundingClientRect();
      tabTooltip.style.left = Math.max(0, Math.min(rect.left, innerWidth - tabTooltip.offsetWidth)) + "px";
      tabTooltip.style.top = Math.max(0, rect.top - tabTooltip.offsetHeight) + "px";
    }, 450);
  });
  win.tab.addEventListener("mouseleave", hideTabTooltip);
  win.tab.addEventListener("pointerdown", hideTabTooltip);
  win.tab.addEventListener("contextmenu", (event) => {
    event.stopPropagation();
    activate(win);
    const en = document.documentElement.lang === "en";
    showContext(event, [
      { label: en ? "Restore" : "Восстановить", disabled: !win.maximized, onclick: () => maximize(win) },
      { label: en ? "Move" : "Передвинуть", disabled: true },
      { label: en ? "Size" : "Размер", disabled: !win.geometry || !!win.maximized, onclick: () => win.geometry?.startResize() },
      { label: en ? "Minimize" : "Свернуть", onclick: () => minimize(win) },
      { label: en ? "Maximize" : "Развернуть", disabled: !!win.maximized, onclick: () => maximize(win) },
      { sep: true },
      { label: en ? "Close" : "Закрыть", onclick: () => closeWindow(win) },
      { sep: true },
      { label: en ? "Dockable" : "Соединяемое", disabled: true },
      { sep: true },
      { label: en ? "Normal" : "Обычное", disabled: true },
      { label: en ? "Floating" : "Свободное", disabled: true },
      { label: en ? "Pinned" : "Прикрепленное", disabled: true },
      { sep: true },
      { label: en ? "Restore window position" : "Восстановить положение окна", onclick: () => {
        if (win.geometry) return win.geometry.reset();
        win.maximized = false; win.frame.classList.remove("maximized");
        for (const prop of ["left", "top", "width", "height"]) win.frame.style.removeProperty(prop);
      } },
      { label: en ? "Next window" : "Следующее окно", disabled: wins.length < 2, onclick: () => activate(wins[(wins.indexOf(win) + 1) % wins.length]) },

    ]);
    const menu = document.querySelector(".context-menu");
    menu.classList.add("mdi-system-menu");
    for (const item of menu.querySelectorAll(".mitem")) {
      const label = item.querySelector(".menu-label").textContent;
      if (label === (en ? "Close" : "Закрыть")) item.classList.add("mdi-default");
      if ([en ? "Dockable" : "Соединяемое", en ? "Normal" : "Обычное"].includes(label)) {
        item.setAttribute("role", "menuitemcheckbox");
        item.setAttribute("aria-checked", "true");
        item.querySelector(".menu-icon").textContent = "✓";
      }
    }
  });
  win.tab.addEventListener("click", () => activate(win));
  win.tab.addEventListener("auxclick", (ev) => { if (ev.button === 1) closeWindow(win); });
  win.tab.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); activate(win); }
  });
  setIcon(win, spec.icon || null);
  wins.push(win);
  $("tabbar").appendChild(win.tab);
  $("windows").appendChild(win.frame);
  if (win.kind === "x-qb") enableFloatingGeometry(win);
  win.ctl = impl.create(win) || null;
  activate(win, fromHash);
  return win;
}

function setIcon(win, key) {
  win.iconKey = key;
  clear(win.iconHost);
  win.iconHost.appendChild(icon(key));
}

export function update(win, changes) {
  if (changes.module !== undefined) win.module = changes.module;
  if (changes.title) {
    win.title = changes.title;
    win.label.textContent = changes.title;
    win.caption.textContent = changes.title;
    win.tab.setAttribute("aria-label", changes.title);
    if (win === active) $("window-caption").textContent = changes.title;
  }
  win.key = keyOf(win.kind, win.path, win.module);
  if (win === active) writeHash();
}

export function activate(win, fromHash) {
  if (!win || win.closed) return;
  const changed = active !== win;
  active = win;
  for (const w of wins) {
    const on = w === win;
    if (on) w.minimized = false;
    w.frame.hidden = !!w.minimized;
    w.pane.hidden = !!w.minimized;
    w.pane.classList.toggle("active", on);
    w.frame.classList.toggle("active", on);
    w.tab.setAttribute("aria-selected", on ? "true" : "false");
    w.tab.classList.toggle("cur", on);
  }
  win.frame.style.zIndex = ++topWindow;
  syncQueryWindowStack();
  win.ctl?.status?.();
  win.tab.scrollIntoView({ block: "nearest", inline: "nearest" });
  if (!fromHash) writeHash();
  if (changed) for (const fn of listeners) fn(win);
}

export function closeWindow(win) {
  if (!win || win.closed) return;
  hideTabTooltip();
  win.closed = true;
  const i = wins.indexOf(win);
  wins.splice(i, 1);
  if (win.ctl && win.ctl.destroy) win.ctl.destroy();
  win.geometry?.destroy();
  win.tab.remove();
  win.frame.remove();
  syncQueryWindowStack();
  if (active === win) {
    active = null;
    const next = wins[Math.min(i, wins.length - 1)];
    if (next) activate(next);
    else {
      writeHash();
      for (const fn of listeners) fn(null);
    }
  }
}

export function maximize(win) {
  if (!win || win.closed) return;
  if (win.geometry) win.geometry.maximize();
  else {
    win.maximized = !win.maximized;
    win.frame.classList.toggle("maximized", win.maximized);
  }
  activate(win);
}
function minimize(win) {
  win.minimized = true;
  win.frame.hidden = true;
  win.pane.hidden = true;
  const next = wins.findLast((w) => w !== win && !w.minimized);
  if (next) activate(next);
  else { active = null; syncQueryWindowStack(); for (const fn of listeners) fn(null); writeHash(); }
}
export function enableFloatingGeometry(win) {
  if (!win.geometry) attachFloatingWindowGeometry(win);
  syncQueryWindowStack();
}

function syncQueryWindowStack() {
  $("windows").classList.toggle("query-window-foreground", !!active && !active.geometry && wins.some((w) => w.geometry && !w.minimized));
}
export function closeAll() {
  for (const w of wins.slice()) closeWindow(w);
}

export function windows() {
  return wins.slice();
}

export function openFromHash() {
  const h = parseHash();
  if (!h) return;
  if (active && location.hash === "#" + hashOf(active)) return;
  openWindow(h, true);
}

export function initTabs() {
  $("layout").after($("tabbar"));
  window.addEventListener("hashchange", openFromHash);
  const hint = document.querySelector("#statusbar span:first-child");
  const fallback = hint.textContent;
  let commandData = new Map();
  fetch("/front/commands.json").then((r) => r.json()).then((data) => {
    function visit(value) {
      if (!value || typeof value !== "object") return;
      if (value.id) commandData.set(value.id, value);
      for (const child of Object.values(value)) if (typeof child === "object") visit(child);
    }
    visit(data);
  });
  document.addEventListener("pointerover", (e) => {
    const command = commandData.get(e.target.closest("[data-command]")?.dataset.command);
    const lang = document.documentElement.lang === "en" ? "en" : "ru";
    hint.textContent = command?.["status_" + lang] || command?.["hint_" + lang] || command?.["description_" + lang] || command?.[lang] || fallback;
  });
  onActive((win) => {
    if (win?.ctl?.status) win.ctl.status();
    else document.querySelector("#statusbar span:last-child").textContent = "CAP　 NUM　 OVR　 ru";
  });
}

export function tabStrip(tabs, label, start, onSelect) {
  const bar = el("div", { class: "otabs", role: "tablist", "aria-label": label || null });
  const body = el("div", { class: "opanels" });
  const btns = [];
  const panels = [];
  let cur = 0;
  const select = (i) => {
    if (!btns.length) return;
    cur = Math.max(0, Math.min(i, btns.length - 1));
    btns.forEach((b, k) => {
      const on = k === cur;
      b.setAttribute("aria-selected", on ? "true" : "false");
      b.tabIndex = on ? 0 : -1;
      b.classList.toggle("cur", on);
      panels[k].hidden = !on;
    });
    if (bar.isConnected && bar.getAttribute("aria-orientation") === "vertical") {
      const selected = btns[cur].getBoundingClientRect();
      const viewport = bar.getBoundingClientRect();
      if (selected.top < viewport.top) bar.scrollTop -= viewport.top - selected.top;
      else if (selected.bottom > viewport.bottom) bar.scrollTop += selected.bottom - viewport.bottom;
    } else if (bar.isConnected) {
      const selected = btns[cur].getBoundingClientRect();
      const viewport = bar.getBoundingClientRect();
      if (selected.left < viewport.left) bar.scrollLeft -= viewport.left - selected.left;
      else if (selected.right > viewport.right) bar.scrollLeft += selected.right - viewport.right;
    }
    if (onSelect) onSelect(cur);
  };
  tabs.forEach((t, i) => {
    const b = el("button", {
      class: "otab", type: "button", role: "tab", "data-title": t.title, title: t.title,
      onclick: () => select(i),
      onkeydown: (ev) => {
        const vertical = bar.getAttribute("aria-orientation") === "vertical";
        const d = ev.key === (vertical ? "ArrowDown" : "ArrowRight") ? 1 : ev.key === (vertical ? "ArrowUp" : "ArrowLeft") ? -1 : 0;
        if (!d) return;
        ev.preventDefault();
        let next = cur;
        for (let step = 0; step < btns.length; step++) {
          next = (next + d + btns.length) % btns.length;
          if (!btns[next].hidden && !btns[next].disabled) break;
        }
        select(next);
        btns[cur].focus();
      },
    }, t.title);
    const p = el("div", { class: "opanel", role: "tabpanel", hidden: true }, t.node);
    btns.push(b);
    panels.push(p);
    bar.appendChild(b);
    body.appendChild(p);
  });
  select(start || 0);
  return { root: el("div", { class: "ostrip" }, bar, body), select, index: () => cur };
}
