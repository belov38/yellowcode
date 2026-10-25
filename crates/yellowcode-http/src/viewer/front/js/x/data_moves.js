import { el, fill } from "../dom.js";
import { createDataGrid } from "./data_grid.js";
import { tx } from "./waiting.js";

const STALE_RESTARTS = 3;

export function movementsNode(ref, get, signal, onRow) {
  const list = el("div", { class: "xc-moves-list" });
  const box = el("details", { class: "xc-moves" },
    el("summary", { class: "xc-moves-title" }, tx("Движения", "Register records")), list);
  const grids = [];
  let asked = false;
  signal.addEventListener("abort", () => {
    for (const g of grids) g.destroy();
    grids.length = 0;
  });
  box.addEventListener("toggle", () => {
    if (!box.open || asked) return;
    asked = true;
    fill(list, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
    get("movements", { ref }, signal).then(
      (v) => {
        if (signal.aborted) return;
        if (!v.found) {
          fill(list, el("div", { class: "msg bad xc-moves-fail" }, tx("Объект не найден: ", "Object not found: ") + ref));
          return;
        }
        if (!v.registers.length) {
          fill(list, el("div", { class: "xd-empty xc-moves-none" }, tx("Документ не делает движений ни по одному регистру",
            "The document records no register")));
          return;
        }
        fill(list, v.registers.map((r) => registerNode(r)));
      },
      (e) => {
        if (e && e.name === "AbortError") return;
        asked = false;
        fill(list, el("div", { class: "msg bad xc-moves-fail", "data-status": e.status || null }, tx("Ошибка: ", "Error: ") + e.message));
      },
    );
  });

  function registerNode(r) {
    const access = r.index ? tx("индекс ", "index ") + r.index : tx("чтение без индекса", "read without an index");
    const title = [
      el("span", { class: "xc-reg-name" }, r.title),
      el("span", { class: "xc-reg-access dim" + (r.index ? "" : " xc-noindex") }, " · " + access),
      r.error ? el("span", { class: "bad xc-reg-err" }, " · " + r.error)
        : r.hasRecords ? null : el("span", { class: "xc-reg-none" }, " · " + tx("нет записей", "no records")),
    ];
    const attrs = { class: "xc-reg", "data-register": r.register, "data-records": r.hasRecords ? "1" : "0" };
    if (!r.hasRecords) {
      attrs.class += " xc-reg-empty";
      return el("div", attrs, el("div", { class: "xc-reg-title" }, title));
    }
    const head = el("summary", { class: "xc-reg-title" }, title);
    const host = el("div", { class: "xc-reg-grid" });
    const note = el("div", { class: "xd-note xc-reg-note", hidden: true });
    const node = el("details", attrs, head, note, host);
    let grid = null;
    let stale = 0;
    let columns = null;
    function start(staleError) {
      if (grid) {
        grid.destroy();
        const at = grids.indexOf(grid);
        if (at >= 0) grids.splice(at, 1);
      }
      grid = null;
      if (staleError) stale++;
      if (stale > STALE_RESTARTS) {
        note.hidden = false;
        note.textContent = tx("Листание остановлено, ответ сервера: ", "The list stopped, the server said: ") + staleError.message;
        fill(host);
        return;
      }
      if (staleError) {
        note.hidden = false;
        note.textContent = tx("Данные изменились, листание начато заново", "The data changed, the list started again");
      }
      grid = createDataGrid({
        fetch: (cursor, s) => get("movements/page", { ref, register: r.register, cursor: cursor || undefined }, s),
        onPage(p, cursor) {
          if (cursor !== null) stale = 0;
          if (!columns) columns = p.columns;
        },
        onStale: (e) => start(e),
        onRowActivate: (i, row) => onRow(r.register, i, columns || [], row),
      });
      grids.push(grid);
      fill(host, grid.root);
    }
    node.addEventListener("toggle", () => {
      if (node.open && !grid) start(null);
    });
    return node;
  }

  return box;
}
