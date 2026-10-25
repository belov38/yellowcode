import { el, fill } from "../dom.js";
import { qget } from "./net.js";
import { kf4a2Text, tx } from "./waiting.js";

const SHOWN_TEXT = 200;

const oneLine = (s, n) => {
  const t = String(s).replace(/\s+/g, " ").trim();
  return t.length > n ? t.slice(0, n) + "…" : t;
};

export function createLiterals(source, onPick) {
  const root = el("div", { class: "xb-lits" });
  let dead = false;
  const status = el("div", { class: "msg" }, tx("Загрузка литералов…", "Loading literals…"));
  const err = el("div", { class: "xb-lit-err", role: "alert" });
  const list = el("div", { class: "xb-lit-list" });
  fill(root, el("div", { class: "xb-head" }, tx("Запросы-литералы модуля ", "Query literals of the module ") + (source.module || source.path)), status, err, list);

  qget("literals", { path: source.path, module: source.module || null }).then(
    (j) => {
      if (dead) return;
      const items = j.literals || [];
      fill(status, items.length ? tx("Литералов: ", "Literals: ") + items.length + (j.truncated ? tx(" (показаны не все)", " (not all are shown)") : "") : tx("В модуле нет литералов-запросов", "The module has no query literals"));
      fill(list, items.map((l) => literalRow(l)));
    },
    (e) => {
      if (!dead) fill(status, el("span", { class: "bad" }, tx("Ошибка: ", "Error: ") + e.message));
    },
  );

  function literalRow(l) {
    const where = el("span", { class: "xb-lit-pos dim" }, "(" + l.line + ", " + l.col + ")");
    const text = el("span", { class: "xb-lit-text" }, oneLine(l.text, SHOWN_TEXT));
    if (l.kind !== "query") {
      const why = l.kind === "composite" ? kf4a2Text() : tx("слишком велик", "too large");
      return el("div", { class: "xb-lit xb-lit-grey", "data-kind": l.kind, "aria-disabled": "true", title: l.kind === "composite" ? tx("Составной запрос: ", "A composite query: ") + kf4a2Text() : why },
        where, text, el("span", { class: "xq-wait xb-lit-wait", "data-wait": l.kind }, why));
    }
    const row = el("button", { class: "xb-lit xb-lit-q", type: "button", "data-kind": "query", title: oneLine(l.text, 500) }, where, text);
    row.addEventListener("click", () => {
      fill(err);
      onPick(l.text).then((e) => {
        if (dead || !e) return;
        fill(err, el("div", { class: "xq-err" }, el("div", { class: "xq-err-head" }, tx("Запрос не открывается", "The query does not open") + (e.at ? " {(" + e.at[0] + ", " + e.at[1] + ")}" : "")),
          (e.chain || []).map((c) => el("div", { class: "xq-err-line" }, String(c)))));
      });
    });
    return row;
  }

  return { root, destroy() { dead = true; } };
}
