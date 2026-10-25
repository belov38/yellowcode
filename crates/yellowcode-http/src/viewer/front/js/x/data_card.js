import { el, fill, portions } from "../dom.js";
import { activate, openWindow, update, windows } from "../tabs.js";
import { cellNode, versionLast } from "./data_grid.js";
import { movementsNode } from "./data_moves.js";
import { cellText } from "./table.js";
import { tx } from "./waiting.js";

export const CARD_KIND = "x-data-card";
const PART_STEP = 500;
const TITLE_LEN = 1000;
const records = new Map();

const cardTitle = (ref) => tx("Карточка: ", "Card: ") + ref;
const recordTitle = (path, n) => tx("Запись: ", "Record: ") + path + tx(", строка ", ", row ") + n;
const isRef = (s) => /^[A-Za-z]+Ref\.[^()]+\(.+\)$/.test(s);

function cget(route, params, signal) {
  const q = Object.keys(params)
    .filter((k) => params[k] !== undefined && params[k] !== null)
    .map((k) => encodeURIComponent(k) + "=" + encodeURIComponent(params[k]));
  return fetch("/api/x/data/" + route + "?" + q.join("&"), { signal }).then((r) =>
    r.json().catch(() => null).then((j) => {
      if (r.ok && j) return j;
      const e = new Error((j && j.error) || String(r.status));
      e.status = r.status;
      throw e;
    }),
  );
}

export function openCard(ref) {
  return openWindow({ kind: CARD_KIND, path: ref, title: cardTitle(ref) });
}

function recordKey(cells) {
  let h = 0x811c9dc5;
  for (const c of cells) {
    const t = c.t + "\u0000";
    for (let i = 0; i < t.length; i++) h = Math.imul(h ^ t.charCodeAt(i), 0x01000193) >>> 0;
  }
  return h.toString(16).padStart(8, "0");
}

export function openRecord(path, index, columns, row) {
  const key = path + "#" + recordKey(row.cells);
  records.set(key, { path, n: index + 1, columns, cells: row.cells, cut: row.cut });
  return openWindow({ kind: CARD_KIND, path: key, title: recordTitle(path, index + 1) });
}

function valueCell(c, j, cut, ref, full) {
  const td = cellNode(c, j, cut);
  if (!cut && c.t.length > TITLE_LEN) {
    td.removeAttribute("title");
    fill(td, el("div", { class: "xc-long" }, td.textContent));
  }
  if (!cut || !full) return td;
  const text = el("span", { class: "xc-text" }, td.textContent);
  const btn = el("button", { class: "btn xc-full", type: "button" }, tx("Показать полностью", "Show in full"));
  const err = el("span", { class: "bad xc-err", hidden: true });
  fill(td, text, " ", btn, err);
  btn.addEventListener("click", () => {
    btn.disabled = true;
    err.hidden = true;
    const { signal, ...ask } = full;
    cget("value", Object.assign({ ref }, ask), signal).then(
      (v) => {
        const whole = v.value || c;
        for (const k of [...td.classList]) if (k === "xd-cut" || k.startsWith("xq-k-")) td.classList.remove(k);
        td.classList.add("xq-k-" + whole.k);
        td.removeAttribute("title");
        fill(td, el("div", { class: "xc-text xc-long" }, cellText(whole, null)));
      },
      (e) => {
        if (e && e.name === "AbortError") return;
        btn.disabled = false;
        err.hidden = false;
        err.textContent = tx("Не получено: ", "Not received: ") + e.message;
      },
    );
  });
  return td;
}

function fieldsTable(columns, values, cutOf, ref, signal) {
  return el("table", { class: "xq-table xc-fields" },
    el("tbody", null, columns.map((col, j) => {
      const cut = cutOf(j);
      const td = valueCell(values[j], j, cut, ref, ref ? { field: col.name, signal } : null);
      td.classList.add("xc-val");
      return el("tr", { class: "xc-field", "data-field": col.name },
        el("th", { class: "xc-name", title: col.ty }, col.name), td);
    })));
}

function partNode(part, ref, signal) {
  const cuts = new Map((part.cut || []).map((c) => [c.row + ":" + c.col, c.length]));
  const lineCol = part.columns.findIndex((c) => c.name === "НомерСтроки");
  const count = part.error ? "" : " (" + part.rows.length + ")";
  const head = el("tr", null, el("th", { class: "xq-rn" }, "№"),
    part.columns.map((c) => el("th", { class: "xq-h xq-k-" + c.kind, title: c.ty, "data-col": c.name },
      el("div", { class: "xq-hn" }, c.name))));
  const tbody = el("tbody");
  const foot = el("div", { class: "xc-part-foot" });
  const box = el("section", { class: "xc-part", "data-part": part.name },
    el("h3", { class: "xc-part-title" }, tx("Табличная часть ", "Tabular part ") + part.name + count),
    part.truncated ? el("div", { class: "xc-part-note" }, part.note || tx("Показаны первые строки: ", "The first rows are shown: ") +
      part.rows.length + tx(" из ", " of ") + part.total +
      tx(" — остальные не уместились в предел памяти результата запроса", " — the rest did not fit the memory limit of a query result")) : null,
    part.error ? el("div", { class: "msg bad xc-part-error" }, part.error) : null,
    part.error ? null : el("div", { class: "xq-scroll xc-part-scroll" }, el("table", { class: "xq-table xc-part-table" }, el("thead", null, head), tbody)),
    foot);
  if (!part.error) {
    if (!part.rows.length) foot.textContent = tx("нет строк", "no rows");
    else {
      portions(tbody, foot, part.rows, (row, i) => {
        const line = lineCol >= 0 ? row[lineCol].t : null;
        return el("tr", null, el("td", { class: "xq-rn" }, i + 1), row.map((c, j) =>
          valueCell(c, j, cuts.get(i + ":" + j), ref, line ? { field: part.columns[j].name, tabular: part.name, line, signal } : null)));
      }, PART_STEP, (rest) => tx("Ещё строк: ", "More rows: ") + rest);
    }
  }
  return box;
}

export function createCardTab(win) {
  const back = el("button", { class: "btn xc-back", type: "button", disabled: true }, tx("Назад", "Back"));
  const where = el("span", { class: "xc-where" });
  const body = el("div", { class: "xc-body" });
  const host = el("div", { class: "xc" }, el("div", { class: "xq-bar xc-bar" }, back, where), body);
  win.pane.appendChild(host);
  const history = [];
  let dead = false;
  let shown = 0;
  let ctl = null;

  function show(path) {
    if (ctl) ctl.abort();
    ctl = new AbortController();
    win.path = path;
    win.pane.dataset.path = path;
    const rec = records.get(path);
    update(win, { title: rec ? recordTitle(rec.path, rec.n) : cardTitle(path) });
    back.disabled = history.length === 0;
    where.textContent = rec ? rec.path : path;
    if (rec) return showRecord(rec);
    if (!isRef(path)) {
      fill(body, el("div", { class: "msg xc-fail" }, tx("Запись регистра открывается из списка: двойной щелчок по строке",
        "A register record opens from the list: double-click its row")));
      return;
    }
    const n = ++shown;
    fill(body, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
    cget("card", { ref: path }, ctl.signal).then(
      (card) => {
        if (!dead && n === shown) showCard(card, path);
      },
      (e) => {
        if (e && e.name === "AbortError") return;
        if (!dead && n === shown) fill(body, el("div", { class: "msg bad xc-fail", "data-status": e.status || null }, tx("Ошибка: ", "Error: ") + e.message));
      },
    );
  }

  function showCard(card, ref) {
    if (!card.found) {
      fill(body, el("div", { class: "msg bad xc-missing" }, tx("Объект не найден: ", "Object not found: ") + ref));
      return;
    }
    where.textContent = card.path;
    versionLast(card.columns, [card.values], card.cut);
    const cuts = new Map((card.cut || []).map((c) => [c.col, c.length]));
    fill(body, card.note ? el("div", { class: "xd-note xc-note xc-areas" }, card.note) : null,
      card.valuesNote ? el("div", { class: "xd-note xc-note xc-values-note" }, card.valuesNote) : null,
      fieldsTable(card.columns, card.values, (j) => cuts.get(j), card.reference, ctl.signal),
      card.tabular.map((p) => partNode(p, card.reference, ctl.signal)),
      card.path.startsWith("Document.") ? movementsNode(card.reference, cget, ctl.signal, openRecord) : null);
  }

  function showRecord(rec) {
    fill(body,
      el("div", { class: "xd-note xc-note" }, tx("Запись показана по строке списка: обрезанные в списке значения не дочитываются",
        "The record is shown from its list row: values cut in the list are not read further")),
      fieldsTable(rec.columns, rec.cells, (j) => rec.cut && rec.cut[j], null, null));
  }

  const holder = (path) => windows().find((w) => w !== win && w.kind === CARD_KIND && w.path === path) || null;

  body.addEventListener("click", (ev) => {
    const td = ev.target.closest && ev.target.closest("td[data-ref]");
    if (!td || ev.target.closest("button")) return;
    const ref = td.dataset.ref;
    if (ref === win.path) return;
    const other = holder(ref);
    if (other) return activate(other);
    history.push(win.path);
    show(ref);
  });
  back.addEventListener("click", () => {
    if (!history.length) return;
    const other = holder(history[history.length - 1]);
    if (other) return activate(other);
    show(history.pop());
  });

  show(win.path);
  return {
    destroy() {
      dead = true;
      if (ctl) ctl.abort();
      for (const p of history.concat(win.path)) records.delete(p);
    },
  };
}
