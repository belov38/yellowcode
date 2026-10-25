import { $, el, fill } from "./dom.js";
import { icon } from "./icon.js";

const menus = [];
let popup = null;
let generation = 0;

export function closeAllMenus() {
  hideTooltip();
  generation++;
  for (const m of menus) m.close();
  if (popup) popup.remove();
  popup = null;
}

export function menuItems(items, close = closeAllMenus) {
  return items.map((it) => {
    if (it.sep) return el("div", { class: "msep", role: "separator" });
    const button = el("button", {
      class: "mitem" + (it.bold ? " menu-default" : "") + (it.missing ? " menu-missing" : ""), "data-accel": it.accel, "data-observed-disabled": it.observedDisabled, "data-unresolved": it.unresolved, type: "button", role: "menuitem", disabled: !!it.disabled, title: it.title,
      "data-command": it.id, "data-key": it.key, "data-effect": it.effect,
      onclick: () => { if (!it.items) { close(); if (it.onclick) it.onclick(); } },
    }, it.checked ? el("span", { class: "menu-icon menu-check" }, "✓") : it.icon ? icon(it.icon) : el("span", { class: "menu-icon" }),
    el("span", { class: "menu-label" }, it.label), el("span", { class: "menu-shortcut" }, it.shortcut || ""));
    if (!it.items) return button;
    const sub = el("div", { class: "mpop submenu", role: "menu", hidden: true });
    if (Array.isArray(it.items)) fill(sub, menuItems(it.items, close));
    const wrap = el("div", { class: "submenu-wrap" }, button, sub);
    button.appendChild(el("span", { class: "menu-arrow", "aria-hidden": "true" }));
    button.setAttribute("aria-haspopup", "true");
    const open = async () => {
      if (it.disabled) return;
      if (typeof it.items === "function") {
        const entries = await it.items();
        if (!wrap.isConnected) return;
        fill(sub, menuItems(entries, close));
      }
      sub.hidden = false;
      const rect = button.getBoundingClientRect();
      sub.style.left = Math.max(0, rect.right + sub.offsetWidth > innerWidth ? rect.left - sub.offsetWidth : rect.right) + "px";
      sub.style.top = Math.max(0, Math.min(rect.top, innerHeight - sub.offsetHeight)) + "px";
    };
    wrap.addEventListener("mouseenter", open);
    wrap.addEventListener("mouseleave", () => { sub.hidden = true; });
    button.addEventListener("click", open);
    button.addEventListener("keydown", (e) => { if (e.key === "ArrowRight") { e.preventDefault(); open().then(() => sub.querySelector("button:not(:disabled)")?.focus()); } });
    return wrap;
  });
}

export function addMenu(id, title, build) {
  const pop = el("div", { class: "mpop", role: "menu", hidden: true });
  const titleOf = typeof title === "function" ? title : () => title;
  const btn = el("button", { class: "mbtn", type: "button", "aria-haspopup": "true", "aria-expanded": "false" }, titleOf());
  const wrap = el("div", { class: "menu", id }, btn, pop);
  const close = () => { pop.hidden = true; btn.setAttribute("aria-expanded", "false"); };
  btn.addEventListener("click", async () => {
    if (!pop.hidden) return closeAllMenus();
    closeAllMenus();
    const mine = generation;
    const items = await build();
    if (mine !== generation) return;
    fill(pop, menuItems(items));
    pop.hidden = false;
    btn.setAttribute("aria-expanded", "true");
  });
  menus.push({ close, wrap, btn, titleOf });
  $("menubar").appendChild(wrap);
  return wrap;
}

export function showContext(event, items, options = {}) {
  event.preventDefault();
  closeAllMenus();
  popup = el("div", { class: "mpop context-menu" + (options.disabledHover ? " menu-disabled-hover" : ""), role: "menu" }, menuItems(items));
  document.body.appendChild(popup);
  popup.style.left = Math.max(0, Math.min(event.clientX, innerWidth - popup.offsetWidth)) + "px";
  popup.style.top = Math.max(0, Math.min(event.clientY, innerHeight - popup.offsetHeight)) + "px";
}

export function retitleMenus() {
  for (const m of menus) m.btn.textContent = m.titleOf();
}

let tooltip = null;
let tooltipTimer = null;
function hideTooltip() {
  clearTimeout(tooltipTimer);
  tooltip?.remove();
  tooltip = null;
}
function toolbarTarget(target) {
  return target.closest(".toolbar button, #tree-actions button");
}
function prepareTooltip(button) {
  if (button.title) {
    button.dataset.tooltip = button.title;
    button.removeAttribute("title");
  }
}
function scheduleTooltip(button) {
  hideTooltip();
  prepareTooltip(button);
  tooltipTimer = setTimeout(() => {
    if (!button.isConnected || !button.dataset.tooltip) return;
    tooltip = el("div", { class: "cfg-tooltip", id: "cfg-tooltip", role: "tooltip" }, button.dataset.tooltip);
    document.body.appendChild(tooltip);
    const rect = button.getBoundingClientRect();
    tooltip.style.left = Math.max(0, Math.min(rect.left, innerWidth - tooltip.offsetWidth)) + "px";
    tooltip.style.top = Math.max(0, rect.bottom + 4 + tooltip.offsetHeight > innerHeight ? rect.top - tooltip.offsetHeight - 4 : rect.bottom + 4) + "px";
  }, 500);
}
export function initMenus() {
  document.addEventListener("pointerover", (event) => {
    const button = toolbarTarget(event.target);
    if (button && !button.contains(event.relatedTarget)) scheduleTooltip(button);
  }, true);
  document.addEventListener("pointerout", (event) => {
    const button = toolbarTarget(event.target);
    if (button && !button.contains(event.relatedTarget)) hideTooltip();
  }, true);
  document.addEventListener("focusin", (event) => { const button = toolbarTarget(event.target); if (button) scheduleTooltip(button); });
  document.addEventListener("focusout", hideTooltip);
  document.addEventListener("pointerdown", hideTooltip, true);
  window.addEventListener("blur", hideTooltip);
  document.addEventListener("scroll", hideTooltip, true);
  document.addEventListener("click", (e) => {
    if (!e.target.closest(".mpop,.mbtn,[data-popup]")) closeAllMenus();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeAllMenus();
    const pop = e.target.closest?.(".mpop") || document.querySelector(".menu > .mpop:not([hidden])");
    if (pop && /^[0-9]$/.test(e.key)) {
      const target = pop.querySelector(`:scope > .mitem[data-accel="${e.key}"]:not(:disabled)`);
      if (target) {
        e.preventDefault();
        target.click();
        return;
      }
    }
    if (!["ArrowDown", "ArrowUp"].includes(e.key) || !e.target.closest(".mpop")) return;
    const buttons = [...e.target.closest(".mpop").querySelectorAll(":scope > .mitem:not(:disabled), :scope > .submenu-wrap > .mitem:not(:disabled)")];
    if (!buttons.length) return;
    e.preventDefault();
    buttons[(buttons.indexOf(e.target) + (e.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length].focus();
  });
}
