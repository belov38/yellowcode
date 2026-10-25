import { el } from "../dom.js";

export function tx(ru, en) {
  return document.documentElement.lang === "en" ? en : ru;
}

export const openBaseText = () =>
  tx(
    "Откройте базу 1CD (файл 1Cv8.1CD): консоль запросов работает на данных базы",
    "Open a 1CD database (the 1Cv8.1CD file): the query console works on the data of the base",
  );

export const waitingEntries = () => [
  { key: "irl", text: tx("IR-L — ждёт Q", "IR-L — waits for Q") },
  { key: "sql", text: tx("SQL — ждёт M9", "SQL — waits for M9") },
];

export function waitLine(text, key) {
  return el("div", { class: "xq-wait", "data-wait": key || null }, text);
}

export function waitingBlock() {
  return el("div", { class: "xq-waitblock" }, waitingEntries().map((w) => waitLine(w.text, w.key)));
}

export function laterStrip(chain) {
  return el("div", { class: "xq-later", role: "status" }, (chain || []).map((t) => el("div", null, String(t))));
}

export function openBaseMessage(reason) {
  return el(
    "div",
    { class: "xq-notbase" },
    el("div", { class: "xq-wait xq-big", "data-wait": "base" }, openBaseText()),
    reason ? el("div", { class: "dim" }, String(reason)) : null,
  );
}

export const kf4a2Text = () => tx("ждёт KF4a2", "waits for KF4a2");
