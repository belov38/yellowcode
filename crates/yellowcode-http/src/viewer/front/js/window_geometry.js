import { el } from "./dom.js";

const MARGIN = 8, MIN_WIDTH = 760, MIN_HEIGHT = 460;
const limit = (value, low, high) => Math.max(low, Math.min(value, high));

export function attachFloatingWindowGeometry(win) {
  const frame = win.frame, layout = document.getElementById("layout");
  const windows = document.getElementById("windows"), tabs = document.getElementById("tabbar");
  let layer = layout.querySelector(".query-window-layer");
  if (!layer) {
    layer = el("div", { class: "query-window-layer" });
    layout.appendChild(layer);
  }
  layer.appendChild(frame);
  frame.classList.add("query-floating-window");
  frame.style.minWidth = "0";
  frame.style.minHeight = "0";
  const lifetime = new AbortController();
  const on = (node, name, fn, options = {}) => node.addEventListener(name, fn, { ...options, signal: lifetime.signal });
  const caption = frame.querySelector(".mdi-caption");
  let rect, normal, gesture = null, keyboard = null;
  const bounds = () => ({ width: layer.clientWidth, height: layer.clientHeight });
  function clamp(value) {
    const area = bounds();
    const width = Math.min(Math.max(0, area.width - 2 * MARGIN), Math.max(MIN_WIDTH, value.width));
    const height = Math.min(Math.max(0, area.height - 2 * MARGIN), Math.max(MIN_HEIGHT, value.height));
    return { left: limit(value.left, MARGIN, area.width - MARGIN - width),
      top: limit(value.top, MARGIN, area.height - MARGIN - height), width, height };
  }
  function paint(value) {
    rect = value;
    for (const prop of ["left", "top", "width", "height"]) frame.style[prop] = value[prop] + "px";
  }
  function apply(value) {
    normal = clamp(value);
    paint(normal);
  }
  function refresh() {
    const area = layout.getBoundingClientRect();
    const bottom = Math.min(area.bottom, windows.getBoundingClientRect().bottom, tabs.getBoundingClientRect().top);
    layer.style.height = Math.max(0, bottom - area.top) + "px";
    if (!rect) return;
    if (win.maximized) paint({ left: 0, top: 0, ...bounds() });
    else apply(normal);
  }
  refresh();
  function initial() {
    const area = bounds();
    const width = Math.min(1120, area.width - 2 * MARGIN), height = Math.min(700, area.height - 2 * MARGIN);
    return { left: (area.width - width) / 2, top: (area.height - height) / 2, width, height };
  }
  apply(initial());

  const en = document.documentElement.lang === "en";
  const hint = el("span", { class: "query-window-hint", id: "query-window-size-" + win.id },
    en ? "Arrow keys resize; Shift speeds up; Enter accepts; Escape cancels."
      : "Стрелки изменяют размер; Shift ускоряет; Enter завершает; Escape отменяет.");
  const size = el("button", { type: "button", class: "mdi-size", "aria-label": "Resize window",
    "aria-describedby": hint.id, "aria-pressed": "false", title: en ? "Size" : "Размер" }, "↘");
  frame.querySelector(".mdi-buttons").prepend(size);
  frame.appendChild(hint);
  function finishKeyboard(cancel = false) {
    if (!keyboard) return;
    const saved = keyboard;
    keyboard = null;
    size.setAttribute("aria-pressed", "false");
    if (cancel) apply(saved);
  }
  function startResize() {
    if (win.maximized || win.closed) return;
    finishKeyboard();
    keyboard = { ...rect };
    size.setAttribute("aria-pressed", "true");
    size.focus({ preventScroll: true });
  }
  on(size, "click", startResize);
  on(size, "blur", () => finishKeyboard());
  on(frame, "keydown", (event) => {
    if (!keyboard || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Enter", "Escape"].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Enter" || event.key === "Escape") return finishKeyboard(event.key === "Escape");
    const step = event.shiftKey ? 40 : 8;
    apply({ ...rect, width: rect.width + (event.key === "ArrowRight" ? step : event.key === "ArrowLeft" ? -step : 0),
      height: rect.height + (event.key === "ArrowDown" ? step : event.key === "ArrowUp" ? -step : 0) });
  }, { capture: true });

  function start(event, edge = "") {
    if (event.button !== 0 || win.maximized || (edge === "" && event.target.closest("button"))) return;
    event.preventDefault();
    finishKeyboard();
    gesture = { target: event.currentTarget, pointer: event.pointerId, x: event.clientX, y: event.clientY, rect: { ...rect }, edge };
    gesture.target.setPointerCapture(event.pointerId);
  }
  function endGesture() {
    if (!gesture) return;
    const { target, pointer } = gesture;
    gesture = null;
    if (target.hasPointerCapture(pointer)) target.releasePointerCapture(pointer);
  }
  on(caption, "pointerdown", (event) => start(event));
  const handles = ["n", "ne", "e", "se", "s", "sw", "w", "nw"].map((edge) => {
    const handle = el("div", { class: "query-window-edge", "data-resize": edge, "aria-hidden": "true" });
    frame.appendChild(handle);
    on(handle, "pointerdown", (event) => start(event, edge));
    on(handle, "lostpointercapture", endGesture);
    return handle;
  });
  on(caption, "lostpointercapture", endGesture);
  on(frame, "pointerup", endGesture);
  on(frame, "pointercancel", endGesture);
  on(frame, "pointermove", (event) => {
    if (!gesture || event.pointerId !== gesture.pointer) return;
    const { x, y, rect: saved, edge } = gesture, dx = event.clientX - x, dy = event.clientY - y;
    if (!edge) return apply({ ...saved, left: saved.left + dx, top: saved.top + dy });
    const area = bounds(), minWidth = Math.min(MIN_WIDTH, area.width - 2 * MARGIN), minHeight = Math.min(MIN_HEIGHT, area.height - 2 * MARGIN);
    let left = saved.left, top = saved.top, right = saved.left + saved.width, bottom = saved.top + saved.height;
    if (edge.includes("w")) left = limit(saved.left + dx, MARGIN, right - minWidth);
    if (edge.includes("e")) right = limit(right + dx, left + minWidth, area.width - MARGIN);
    if (edge.includes("n")) top = limit(saved.top + dy, MARGIN, bottom - minHeight);
    if (edge.includes("s")) bottom = limit(bottom + dy, top + minHeight, area.height - MARGIN);
    apply({ left, top, width: right - left, height: bottom - top });
  });
  const observer = new ResizeObserver(refresh);
  for (const node of [layout, windows, tabs]) observer.observe(node);
  on(window, "resize", refresh);
  return win.geometry = {
    startResize,
    maximize() {
      finishKeyboard();
      endGesture();
      win.maximized = !win.maximized;
      frame.classList.toggle("maximized", win.maximized);
      size.disabled = win.maximized;
      refresh();
    },
    reset() {
      finishKeyboard();
      endGesture();
      win.maximized = false;
      frame.classList.remove("maximized");
      size.disabled = false;
      apply(initial());
    },
    destroy() {
      endGesture();
      observer.disconnect();
      lifetime.abort();
      for (const node of [...handles, size, hint]) node.remove();
      if (layer.children.length === 1) layer.remove();
    },
  };
}
