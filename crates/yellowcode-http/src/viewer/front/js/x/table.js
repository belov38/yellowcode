import { el } from "../dom.js";
import { laterStrip, tx } from "./waiting.js";
import { isErrorOut } from "./net.js";

export const PAGE = 200;

function unquote(t, q) {
  if (t.length >= 2 && t.startsWith(q) && t.endsWith(q)) return t.slice(1, -1).split(q + q).join(q);
  return t;
}

export function cellText(c, pr) {
  if (c.k === "string") return unquote(c.t, '"');
  if (c.k === "date") return unquote(c.t, "'");
  if (c.k === "ref" && pr && pr.source !== "canonical") return pr.text;
  return c.t;
}

function sourceNote(pr) {
  if (!pr) return "";
  if (pr.source === "presentation") return tx("представление", "presentation");
  if (pr.source === "fields") return tx("представление заглушки (поля)", "stub presentation (fields)");
  return "";
}

function cellNode(c, pr, nestedOpen) {
  const td = el("td", { class: "xq-c xq-k-" + c.k });
  const note = sourceNote(pr);
  td.title = note ? c.t + "\n" + note : c.t;
  if (c.k === "null" || c.k === "undefined") {
    td.appendChild(el("span", { class: "xq-nul" }, c.t));
  } else if (c.k === "ref" && c.r && pr && pr.source !== "canonical") {
    td.appendChild(el("span", { class: "xq-refkind" }, c.r.kind));
    td.appendChild(el("span", { class: "xq-refpr", "data-source": pr.source }, pr.text));
  } else if (c.k === "nested") {
    const m = /\d+/.exec(c.t);
    const a = el("a", { class: "xq-nested-link", href: "#", role: "button" }, tx("таблица", "table") + (m ? " (" + m[0] + ")" : ""));
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      nestedOpen(td);
    });
    td.appendChild(a);
  } else {
    td.textContent = cellText(c, null);
  }
  return td;
}

export function createTable(opts) {
  const cols = opts.columns;
  const kept = opts.kept === undefined ? opts.total : opts.kept;
  const data = [];
  const trs = [];
  const selected = new Set();
  let ended = false;
  let loading = false;
  let last = -1;
  let dead = false;

  const head = el("tr", null, el("th", { class: "xq-rn" }, "№"));
  cols.forEach((c) => {
    const w = Math.max(6, Math.min(c.width || 10, 40));
    head.appendChild(
      el("th", {
        class: "xq-h xq-k-" + c.kind, style: "min-width:" + w + "ch", title: c.ty + (c.width ? " · " + tx("ширина ", "width ") + c.width : ""),
        "data-col": c.name,
      }, el("div", { class: "xq-hn" }, c.name), el("div", { class: "xq-ht" }, c.ty)),
    );
  });
  const tbody = el("tbody");
  const table = el("table", { class: "xq-table" }, el("thead", null, head), tbody);
  const scroller = el("div", { class: "xq-scroll" }, table);
  const status = el("span", { class: "xq-tstat dim" });
  const copyBtn = el("button", { class: "btn xq-copy", type: "button", onclick: () => copy() }, tx("Копировать как TSV", "Copy as TSV"));
  const note = el("span", { class: "dim xq-pnote" });
  const strip = opts.truncated ? el("div", { class: "xq-cut", role: "status" }) : null;
  const foot = el("div", { class: "xq-tfoot" }, status, note, copyBtn);
  const root = el("div", { class: "xq-tblwrap" }, strip, scroller, foot);

  function refreshStatus() {
    status.textContent = tx("загружено ", "loaded ") + data.length + tx(" из ", " of ") + kept;
    if (strip) {
      strip.textContent = tx("в таблице ", "in the table: ") + kept + tx(" из ", " of ") + opts.total + tx(" (потолок)", " (limit)");
    }
    copyBtn.title = selected.size ? tx("Выбрано строк: ", "Selected rows: ") + selected.size : tx("Все загруженные строки", "All loaded rows");
  }

  function addRow(cells, pres, i) {
    const tr = el("tr", { class: "xq-row", "data-row": i }, el("td", { class: "xq-rn" }, i + 1));
    cells.forEach((c, j) => {
      const td = cellNode(c, pres ? pres[j] : null, (cell) => toggleNested(tr, i, j, cell));
      td.dataset.col = j;
      tr.appendChild(td);
    });
    tr.addEventListener("click", (ev) => {
      if (ev.target.closest && ev.target.closest("a")) return;
      select(i, tr, ev);
    });
    trs[i] = tr;
    return tr;
  }

  function select(i, tr, ev) {
    if (ev.shiftKey && last >= 0) {
      const [a, b] = last < i ? [last, i] : [i, last];
      for (let k = a; k <= b; k++) mark(k, true);
    } else {
      mark(i, !selected.has(i));
      last = i;
    }
    refreshStatus();
  }

  function mark(i, on) {
    if (on) selected.add(i);
    else selected.delete(i);
    const tr = trs[i];
    if (tr) tr.classList.toggle("sel", on);
  }

  function toggleNested(tr, i, j, cell) {
    const next = tr.nextSibling;
    if (next && next.classList && next.classList.contains("xq-nested-row")) {
      next.remove();
      return;
    }
    const box = el("td", { class: "xq-nested-cell", colspan: cols.length + 1 }, el("span", { class: "dim" }, tx("Загрузка…", "Loading…")));
    tr.after(el("tr", { class: "xq-nested-row" }, box));
    const p = opts.nested ? opts.nested(i, j) : null;
    const fail = (m) => {
      box.textContent = "";
      box.appendChild(el("div", { class: "msg bad" }, m));
    };
    if (!p) return fail(tx("вложенная таблица недоступна", "the nested table is not available"));
    p.then(
      (a) => {
        if (dead) return;
        box.textContent = "";
        if (isErrorOut(a)) {
          box.appendChild(a.phase === "later" ? laterStrip(a.chain) : el("div", { class: "msg bad" }, a.chain.join("\n")));
        } else {
          const rows = a.rows || [];
          const sub = createTable({
            columns: a.columns || [], total: rows.length, truncated: false,
            fetch: (from, limit) => Promise.resolve({ rows: rows.slice(from, from + limit), present: [], present_errors: [] }),
            nested: null,
          });
          box.appendChild(sub.root);
        }
      },
      (e) => fail(e.message),
    );
  }

  const shown = () => scroller.offsetParent !== null && scroller.clientHeight > 0;
  const needMore = () => shown() && (scroller.scrollHeight <= scroller.clientHeight + 60 || scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 300);

  function more() {
    if (loading || ended || dead) return;
    loading = true;
    opts.fetch(data.length, PAGE).then(
      (p) => {
        loading = false;
        if (dead) return;
        const frag = document.createDocumentFragment();
        const rows = p.rows || [];
        rows.forEach((cells, k) => {
          const pres = (p.present && p.present[k]) || null;
          data.push({ cells, present: pres });
          frag.appendChild(addRow(cells, pres, data.length - 1));
        });
        tbody.appendChild(frag);
        if (rows.length < PAGE || data.length >= kept) ended = true;
        if (p.present_errors && p.present_errors.length && !note.textContent) {
          note.textContent = tx("Представления ссылок: ", "Reference presentations: ") + p.present_errors[0];
          note.title = p.present_errors.join("\n");
        }
        refreshStatus();
        if (!ended && needMore()) more();
      },
      (e) => {
        loading = false;
        ended = true;
        if (dead) return;
        foot.insertBefore(el("span", { class: "bad" }, e.message), status);
      },
    );
  }

  scroller.addEventListener("scroll", () => {
    if (needMore()) more();
  });
  let watcher = null;
  if (typeof IntersectionObserver !== "undefined") {
    watcher = new IntersectionObserver(() => {
      if (needMore()) more();
    });
    watcher.observe(scroller);
  }

  function tsv() {
    const esc = (s) => String(s).replace(/[\t\r\n]+/g, " ");
    const idx = selected.size ? [...selected].sort((a, b) => a - b) : data.map((_, i) => i);
    const lines = [cols.map((c) => esc(c.name)).join("\t")];
    for (const i of idx) {
      const d = data[i];
      if (d) lines.push(d.cells.map((c, j) => esc(cellText(c, d.present ? d.present[j] : null))).join("\t"));
    }
    return lines.join("\n");
  }

  function fallbackCopy(text) {
    const ta = el("textarea", { class: "xq-hidden-copy" });
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch (e) {
    }
    ta.remove();
  }

  function copy() {
    const text = tsv();
    let done = false;
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
        done = true;
      }
    } catch (e) {
      done = false;
    }
    if (!done) fallbackCopy(text);
    copyBtn.dataset.copied = String(selected.size || data.length);
  }

  refreshStatus();
  more();
  return {
    root, tsv, copy,
    selectedCount: () => selected.size,
    loaded: () => data.length,
    destroy() {
      dead = true;
      if (watcher) watcher.disconnect();
    },
  };
}
