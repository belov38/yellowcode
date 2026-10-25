import { el } from "../dom.js";
import { cellText } from "./table.js";
import { tx } from "./waiting.js";

const KEEP = 8;
const MARGIN = 40;
const SLACK = 10;
const ROW_H = 21;
const MAX_HEIGHT = 15000000;
const LAYOUT_PX = 32000000;
export const REF_DELAY = 250;
const NUM_DIGITS = 7;

export const layoutCap = (dpr, override, rowH = ROW_H, portion = 0) =>
  override || Math.min(MAX_HEIGHT, LAYOUT_PX / (dpr > 0 ? dpr : 1) - portion * rowH);

export const cutTitle = (length, kind) =>
  tx("обрезано, ", "cut, ") + length + (kind === "binary" ? tx(" байт", " bytes") : tx(" символов", " characters"));

const EMPTY_UUID = "00000000-0000-0000-0000-000000000000";

export const cellRef = (c) => (c && c.r && c.r.kind && c.r.id && c.r.id !== EMPTY_UUID ? c.r.kind + "(" + c.r.id + ")" : null);

const VERSION_COLUMN = "ВерсияДанных";

export function versionLast(columns, rows, cut) {
  const at = columns.findIndex((c) => c.name === VERSION_COLUMN);
  const last = columns.length - 1;
  if (at < 0 || at === last) return;
  const move = (a) => {
    if (a.length > at) a.push(a.splice(at, 1)[0]);
  };
  move(columns);
  for (const row of rows) move(row);
  for (const c of cut || []) c.col = c.col === at ? last : c.col > at ? c.col - 1 : c.col;
}

function headCell(c) {
  const w = Math.max(6, Math.min(c.width || 12, 40));
  return el("th", { class: "xq-h xq-k-" + c.kind, style: "min-width:" + w + "ch", title: c.ty, "data-col": c.name },
    el("div", { class: "xq-hn" }, c.name), el("div", { class: "xq-ht" }, c.ty));
}

export function cellNode(c, j, cut) {
  const td = el("td", { class: "xq-c xq-k-" + c.k, "data-col": j });
  if (c.k === "null" || c.k === "undefined") {
    td.appendChild(el("span", { class: "xq-nul" }, c.t));
  } else if (cut && c.k === "string" && c.t.startsWith('"')) {
    td.textContent = c.t.slice(1).split('""').join('"') + "…";
  } else {
    td.textContent = cellText(c, null) + (cut ? "…" : "");
  }
  const ref = cellRef(c);
  if (ref) {
    td.dataset.ref = ref;
    td.classList.add("xd-ref");
  }
  if (cut) {
    td.classList.add("xd-cut");
    td.title = cutTitle(cut, c.k);
  } else {
    td.title = c.t;
  }
  return td;
}

function timing(t0, rows) {
  try {
    performance.measure("xd-portion", { start: t0, end: performance.now(), detail: { rows } });
    performance.clearMeasures("xd-portion");
  } catch (_) {
  }
}

function cutMap(p) {
  const m = new Map();
  for (const c of p.cut || []) m.set(c.row + ":" + c.col, c.length);
  return m;
}

export function createDataGrid(opts) {
  let columns = null;
  let cursor = null;
  let count = 0;
  const parts = [];
  let used = 0;
  let loading = false;
  let ended = false;
  let stopped = false;
  let dead = false;
  let ctl = null;
  let rowH = ROW_H;
  let headH = 0;
  let measured = false;
  let rs = 0;
  let re = 0;
  let override = null;
  let capped = false;
  let maxPortion = 0;
  let failed = null;
  let cur = -1;
  let refTimer = 0;
  const filtered = !!opts.filtered;
  let scanned = 0;
  let searching = false;
  let paused = false;

  const headRow = el("tr", null, el("th", { class: "xq-rn" }, "№"));
  const padCell = () => el("td", { colspan: 1 });
  const padTop = el("tr", { class: "xd-pad", "aria-hidden": "true" }, padCell());
  const padBot = el("tr", { class: "xd-pad", "aria-hidden": "true" }, padCell());
  const tbody = el("tbody", null, padTop, padBot);
  const table = el("table", { class: "xq-table xd-table" }, el("thead", null, headRow), tbody);
  const scroller = el("div", { class: "xq-scroll xd-scroll", tabindex: "0" }, table);
  const empty = el("div", { class: "xd-empty", hidden: true }, tx("нет строк", "no rows"));
  const stat = el("span", { class: "xd-stat dim", "aria-live": "polite" });
  const err = el("span", { class: "bad xd-err" });
  const stopBtn = el("button", { class: "btn xd-stop", type: "button", hidden: true, onclick: () => pause() }, tx("Остановить", "Stop"));
  const goBtn = el("button", { class: "btn xd-go", type: "button", hidden: true, onclick: () => resume() }, tx("Продолжить поиск", "Search on"));
  const root = el("div", { class: "xq-tblwrap xd-grid" }, scroller, empty,
    el("div", { class: "xq-tfoot xd-foot" }, stat, stopBtn, goBtn, err));

  function setState(s) {
    if (s === "loading" && searching) s = "search";
    stat.dataset.state = s;
    stat.dataset.rows = String(count);
    stat.dataset.scanned = filtered ? String(scanned) : "";
    const loaded = tx("загружено строк: ", "rows loaded: ") + count;
    const seen = filtered ? " · " + tx("просмотрено: ", "scanned: ") + scanned : "";
    stat.textContent = {
      loading: count ? loaded + seen + " · " + tx("загрузка…", "loading…") : tx("Загрузка…", "Loading…"),
      search: (count ? loaded + " · " : "") + tx("поиск… просмотрено ", "searching… scanned ") + scanned + tx(" строк", " rows"),
      paused: loaded + seen + " · " + tx("поиск остановлен", "search stopped"),
      more: loaded + seen,
      end: loaded + seen + " · " + tx("все строки", "all rows"),
      empty: tx("нет строк", "no rows") + seen,
      error: loaded + seen,
      cap: loaded + seen + " · " + tx("Показаны первые " + count + " строк — дальше листание недоступно; уточните отбор (период или условия)",
        "the first " + count + " rows are shown — the list cannot scroll further; narrow the selection (period or conditions)"),
    }[s];
    stopBtn.hidden = s !== "search";
    goBtn.hidden = s !== "paused";
  }
  const restState = () => setState(ended ? (count ? "end" : "empty") : capped ? "cap" : paused ? "paused" : "more");

  function pause() {
    paused = true;
    if (!loading) restState();
  }

  function resume() {
    paused = false;
    restState();
    pump();
  }

  function partOf(i) {
    let lo = 0;
    let hi = parts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (parts[mid].start <= i) lo = mid;
      else hi = mid - 1;
    }
    return parts[lo];
  }

  const touches = (q, a, b) => q.start < b && q.start + q.n > a;

  function evict() {
    const kept = parts.filter((q) => q.rows && !touches(q, rs, re));
    let over = kept.length - KEEP;
    kept.sort((a, b) => a.used - b.used);
    for (const q of kept) {
      if (over <= 0) break;
      q.rows = null;
      q.cut = null;
      over--;
    }
  }

  const shown = () => scroller.offsetParent !== null && scroller.clientHeight > 0;
  const nearEnd = () => scroller.scrollHeight <= scroller.clientHeight + 60 || scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 300;

  function view() {
    const top = scroller.scrollTop - headH;
    const first = Math.max(0, Math.min(count, Math.floor(top / rowH)));
    const last = Math.max(first, Math.min(count, Math.ceil((top + scroller.clientHeight) / rowH)));
    return [first, last];
  }

  function rowNode(i) {
    const q = partOf(i);
    const num = el("td", { class: "xq-rn" }, i + 1);
    if (!q.rows) {
      return el("tr", { class: "xd-hole", "data-row": i }, num,
        el("td", { class: "xd-holecell" + (failed ? " bad" : ""), colspan: columns.length }, failed || tx("загрузка…", "loading…")));
    }
    q.used = ++used;
    const k = i - q.start;
    const tr = el("tr", { class: i === cur ? "xd-row xd-cur" : "xd-row", "data-row": i }, num);
    q.rows[k].forEach((c, j) => tr.appendChild(cellNode(c, j, q.cut.get(k + ":" + j))));
    return tr;
  }

  function rowsFrag(a, b) {
    const f = document.createDocumentFragment();
    for (let i = a; i < b; i++) f.appendChild(rowNode(i));
    return f;
  }

  function setPads() {
    padTop.style.height = rs * rowH + "px";
    padBot.style.height = (count - re) * rowH + "px";
    padTop.hidden = rs === 0;
    padBot.hidden = re === count;
  }

  function drawRange(a, b) {
    if (b <= rs || a >= re) {
      while (padTop.nextSibling !== padBot) padTop.nextSibling.remove();
      tbody.insertBefore(rowsFrag(a, b), padBot);
    } else {
      for (; rs < a; rs++) padTop.nextSibling.remove();
      for (; re > b; re--) padBot.previousSibling.remove();
      if (a < rs) tbody.insertBefore(rowsFrag(a, rs), padTop.nextSibling);
      if (b > re) tbody.insertBefore(rowsFrag(re, b), padBot);
    }
    rs = a;
    re = b;
    setPads();
  }

  function measure() {
    const tr = padTop.nextSibling;
    if (measured || tr === padBot) return;
    measured = true;
    const h = tr.getBoundingClientRect().height;
    if (h > 0) rowH = h;
    headH = headRow.getBoundingClientRect().height;
    const num = tr.firstChild;
    const shownNum = num.textContent;
    num.textContent = "9".repeat(NUM_DIGITS);
    const widths = [...headRow.children].map((th, j) => Math.ceil(th.getBoundingClientRect().width) + (j ? 0 : 1));
    num.textContent = shownNum;
    const cols = el("colgroup", null, widths.map((w) => {
      const c = el("col");
      c.style.width = w + "px";
      return c;
    }));
    table.insertBefore(cols, table.firstChild);
    table.style.width = widths.reduce((s, w) => s + w, 0) + "px";
    table.classList.add("xd-fixed");
  }

  function render() {
    if (!columns || !shown()) return;
    if (!measured && count) {
      drawRange(0, Math.min(count, Math.ceil(scroller.clientHeight / rowH) + MARGIN));
      measure();
    }
    const [first, last] = view();
    const lo = Math.max(0, first - SLACK);
    const hi = Math.min(count, last + SLACK);
    if (rs <= lo && re >= hi) {
      setPads();
      return;
    }
    drawRange(Math.max(0, first - MARGIN), Math.min(count, last + MARGIN));
  }

  function redraw(q) {
    const a = Math.max(rs, q.start);
    const b = Math.min(re, q.start + q.n);
    for (let i = a; i < b; i++) tbody.replaceChild(rowNode(i), tbody.children[i - rs + 1]);
  }

  function holeInView() {
    if (!shown() || !count) return null;
    const [first, last] = view();
    const a = Math.max(0, first - SLACK);
    const b = Math.min(count, last + SLACK);
    for (let i = a; i < b;) {
      const q = partOf(i);
      if (!q.rows) return q;
      i = q.start + q.n;
    }
    return null;
  }

  function checkCap() {
    const was = capped;
    capped = !ended && count > 0 && count * rowH >= layoutCap(window.devicePixelRatio, override, rowH, maxPortion);
    return was !== capped;
  }

  function pump() {
    if (loading || stopped || dead) return;
    if (checkCap()) restState();
    const hole = holeInView();
    if (hole) read(hole);
    else if (!paused && !ended && !capped && (!columns || (shown() && nearEnd()))) read(null);
  }

  function changed() {
    stopped = true;
    const e = new Error(tx("данные изменились", "the data changed"));
    if (opts.onStale) opts.onStale(e);
    else {
      setState("error");
      err.textContent = e.message;
    }
  }

  function read(q) {
    loading = true;
    setState("loading");
    const asked = q ? q.cursor : cursor;
    ctl = new AbortController();
    opts.fetch(asked, ctl.signal).then(
      (p) => {
        loading = false;
        if (dead) return;
        const t0 = performance.now();
        const rows = p.rows || [];
        maxPortion = Math.max(maxPortion, p.limit || rows.length);
        if (q) {
          if (rows.length !== q.n || (p.nextCursor || null) !== q.next) return changed();
          q.rows = rows;
          q.cut = cutMap(p);
          q.used = ++used;
          redraw(q);
          render();
          evict();
          restState();
        } else {
          if (!columns) {
            columns = p.columns || [];
            columns.forEach((c) => headRow.appendChild(headCell(c)));
            for (const pad of [padTop, padBot]) pad.firstChild.colSpan = columns.length + 1;
          }
          const next = p.nextCursor || null;
          if (rows.length) {
            parts.push({ start: count, n: rows.length, cursor: asked, next, rows, cut: cutMap(p), used: ++used });
            count += rows.length;
          }
          cursor = next;
          ended = cursor === null;
          if (filtered) {
            scanned += p.scanned || 0;
            searching = !rows.length && !ended;
          }
          render();
          evict();
          if (override === null) override = parseFloat(getComputedStyle(root).getPropertyValue("--xd-max-height")) || 0;
          checkCap();
          if (ended && count === 0) {
            empty.hidden = false;
            scroller.hidden = true;
          }
          restState();
          if (opts.onPage) opts.onPage(p, asked);
        }
        void scroller.scrollHeight;
        timing(t0, rows.length);
        pump();
      },
      (e) => {
        loading = false;
        if (dead || (e && e.name === "AbortError")) return;
        stopped = true;
        if (e && e.status === 400 && asked !== null && opts.onStale) {
          opts.onStale(e);
          return;
        }
        setState("error");
        failed = (e && e.message) || String(e);
        err.textContent = failed;
        for (const td of tbody.querySelectorAll("td.xd-holecell")) {
          td.textContent = failed;
          td.classList.add("bad");
        }
      },
    );
  }

  const onView = () => {
    render();
    pump();
  };
  let lastTop = 0;
  scroller.addEventListener("scroll", () => {
    const down = scroller.scrollTop > lastTop;
    lastTop = scroller.scrollTop;
    if (paused && down && scroller.scrollHeight > scroller.clientHeight && nearEnd()) {
      paused = false;
      restState();
    }
    onView();
  });
  const drawnRow = (i) => tbody.querySelector('tr.xd-row[data-row="' + i + '"]');

  function setCur(i, show) {
    if (!(i >= 0 && i < count)) return;
    const old = drawnRow(cur);
    if (old) old.classList.remove("xd-cur");
    cur = i;
    const tr = drawnRow(i);
    if (tr) tr.classList.add("xd-cur");
    if (show) {
      const top = i * rowH;
      const view = scroller.clientHeight - headH;
      if (top < scroller.scrollTop) scroller.scrollTop = top;
      else if (top + rowH > scroller.scrollTop + view) scroller.scrollTop = top + rowH - view;
    }
  }

  function activate(i) {
    const row = rowAt(i);
    if (row && opts.onRowActivate) opts.onRowActivate(i, row);
  }

  const rowOf = (ev) => {
    const tr = ev.target.closest && ev.target.closest("tr.xd-row");
    return tr ? Number(tr.dataset.row) : -1;
  };
  const cancelRef = () => {
    clearTimeout(refTimer);
    refTimer = 0;
  };
  tbody.addEventListener("click", (ev) => {
    const i = rowOf(ev);
    if (i < 0) return;
    setCur(i, false);
    cancelRef();
    const td = ev.target.closest("td[data-ref]");
    if (!td || !opts.onRefActivate || ev.detail > 1) return;
    const ref = td.dataset.ref;
    refTimer = setTimeout(() => {
      refTimer = 0;
      if (!dead) opts.onRefActivate(ref);
    }, REF_DELAY);
  });
  tbody.addEventListener("dblclick", (ev) => {
    cancelRef();
    const i = rowOf(ev);
    if (i >= 0) activate(i);
  });
  scroller.addEventListener("keydown", (ev) => {
    if (cur < 0 || ev.altKey || ev.ctrlKey || ev.metaKey) return;
    if (ev.key === "Enter") activate(cur);
    else if (ev.key === "ArrowDown") setCur(Math.min(count - 1, cur + 1), true);
    else if (ev.key === "ArrowUp") setCur(Math.max(0, cur - 1), true);
    else return;
    ev.preventDefault();
  });
  const watchers = [];
  if (typeof IntersectionObserver !== "undefined") {
    const io = new IntersectionObserver(onView);
    io.observe(scroller);
    watchers.push(io);
  }
  if (typeof ResizeObserver !== "undefined") {
    const ro = new ResizeObserver(() => requestAnimationFrame(onView));
    ro.observe(scroller);
    watchers.push(ro);
  }

  function rowAt(i) {
    if (!(i >= 0 && i < count)) return null;
    const q = partOf(i);
    if (!q.rows) return null;
    const k = i - q.start;
    const cells = q.rows[k].map((c) => (c.r ? Object.assign({}, c, { r: Object.assign({}, c.r) }) : Object.assign({}, c)));
    return { cells, refs: cells.map(cellRef), cut: cells.map((_, j) => q.cut.get(k + ":" + j) || null) };
  }

  setState("loading");
  pump();
  return {
    root,
    loaded: () => count,
    rowAt,
    destroy() {
      dead = true;
      cancelRef();
      if (ctl) ctl.abort();
      for (const w of watchers) w.disconnect();
    },
  };
}
