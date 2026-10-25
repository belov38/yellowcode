import { icon } from "../icon.js";
import { el, fill, clear } from "../dom.js";
import { closeWindow, openWindow, tabStrip, update } from "../tabs.js";
import { qget, qpost, xget } from "./net.js";
import { createFieldsTab } from "./qb_fields.js";
import { createLiterals } from "./qb_literals.js";
import {
  createBatchTab, createConditionsTab, createDcsTab, createGroupTab, createLinksTab, createModifiersTab, createOrderTab, createTotalsTab, createUnionsTab,
} from "./qb_tabs.js";
import { tx } from "./waiting.js";

export const QB_KIND = "x-qb";
export const QB_PATH = "x/qb";
export const OK_EVENT = "x-qb-ok";

const LINE_H = 18;
const PAD = 4;
const TEXT_TAB = 9;

let pending = null;
let counter = 0;

export function openBuilder(init) {
  pending = init || {};
  const win = openWindow({ kind: QB_KIND, path: QB_PATH, module: String(++counter), title: tx("Конструктор запроса", "Query builder") });
  pending = null;
  return win;
}

export function createQbTab(win) {
  const init = pending || {};
  pending = null;
  update(win, { title: tx("Конструктор запроса", "Query builder") });
  win.caption.prepend(icon("pic/QueryWizard"), " ");
  const host = el("div", { class: "xb-host" }, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
  win.pane.appendChild(host);
  let ctl = null;
  xget("info").then(
    (info) => build(!!info && info.enabled !== false && !info.error),
    () => build(false),
  );
  function build(canRun) {
    if (win.closed) return;
    ctl = buildWindow(win, host, canRun, init);
  }
  return { destroy() { if (ctl) ctl.destroy(); else win.closed = true; } };
}

const fallbackCopy = (text) => {
  const ta = el("textarea", { class: "xq-hidden-copy" });
  ta.value = text;
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand("copy");
  } catch (e) {
  }
  ta.remove();
};

function copyText(text) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
      return;
    }
  } catch (e) {
  }
  fallbackCopy(text);
}

function buildWindow(win, host, canRun, init) {
  let dead = false;
  let ready = false;
  let delivering = false;
  const st = { id: null, text: "", view: null, avail: null, cur: { stmt: 0, op: 0, nested: [] }, dirtyText: false };
  let viewSeq = 0;
  let availSeq = 0;
  let chain = Promise.resolve();
  const tableCache = new Map();
  const dirty = new Set();

  const call = (route, body) =>
    qpost(route, body).then(
      (j) => (j && j.error && typeof j.error === "object" ? { ok: false, err: j.error } : { ok: true, j }),
      (e) => ({ ok: false, err: { chain: [e.message], at: null, fatal: true } }),
    );

  const errBox = el("div", { class: "xb-errs", "aria-live": "polite" });
  const status = el("span", { class: "xb-status dim", "aria-live": "polite" });

  function clearErr() {
    clear(errBox);
    status.textContent = "";
  }

  function showErr(err, opts) {
    const head = (opts && opts.head) || tx("Ошибка", "Error");
    const lines = (err.chain || []).map((c, i) => el("div", { class: "xq-err-line", style: "padding-left:" + i * 12 + "px" }, String(c)));
    fill(errBox, el("div", { class: "xq-err xb-err", role: "alert", "data-at": err.at ? err.at.join(",") : null },
      el("div", { class: "xq-err-head" }, head + (err.at ? " {(" + err.at[0] + ", " + err.at[1] + ")}" : "")),
      lines,
      err.fatal && st.id !== null ? el("button", { class: "btn xb-btn xb-reopen", type: "button", onclick: () => reopen() }, tx("Открыть заново", "Open again")) : null));
  }

  const fail = (text) => {
    showErr({ chain: [text], at: null });
    return Promise.resolve(false);
  };

  async function openText(text) {
    const r = await call("open", { text, mode: "query" });
    if (dead) {
      if (r.ok) qpost("close", { id: r.j.id }).catch(() => {});
      return null;
    }
    if (!r.ok) return r.err;
    const old = st.id;
    st.id = r.j.id;
    st.cur = { stmt: 0, op: 0, nested: [] };
    tableCache.clear();
    if (old !== null) qpost("close", { id: old }).catch(() => {});
    await changed(r.j);
    return null;
  }

  function reopen() {
    if (dead || delivering) return;
    clearErr();
    const text = textTab.value() || st.text;
    chain = chain.then(async () => {
      const e = await openText(text);
      if (e) showErr(e);
    });
  }

  async function changed(j) {
    st.text = j.text;
    st.dirtyText = false;
    textTab.setText(st.text);
    tableCache.clear();
    await refreshView(j.view);
    loadAvail();
  }

  async function fetchView(c) {
    const params = { id: st.id, stmt: c.stmt, op_index: c.op, nested: c.nested.length ? JSON.stringify(c.nested) : null };
    try {
      const v = await qget("view", params);
      return v && v.error ? undefined : v;
    } catch (e) {
      return undefined;
    }
  }

  async function refreshView(hint) {
    const mine = ++viewSeq;
    const isHome = (c) => c.stmt === 0 && c.op === 0 && c.nested.length === 0;
    let v;
    if (hint !== undefined && isHome(st.cur)) v = hint;
    else {
      v = await fetchView(st.cur);
      if (mine !== viewSeq || dead) return;
      if (v === undefined) {
        st.cur = { stmt: 0, op: 0, nested: [] };
        v = hint !== undefined ? hint : await fetchView(st.cur);
        if (mine !== viewSeq || dead) return;
      }
    }
    st.view = v === undefined ? null : v;
    renderAll();
  }

  function loadAvail() {
    const mine = ++availSeq;
    const c = st.cur;
    qget("available", { id: st.id, stmt: c.stmt, op_index: c.op }).then(
      (a) => {
        if (mine !== availSeq || dead || (a && a.error)) return;
        st.avail = a;
        tabs.forEach((t, i) => {
          if (i === 0 && strip.index() === 0) t.render();
          else if (i !== strip.index()) dirty.add(i);
        });
      },
      () => {},
    );
  }

  function applyOp(op) {
    if (dead || delivering || st.id === null) return Promise.resolve(false);
    const run = chain.then(async () => {
      if (dead) return false;
      clearErr();
      const r = await call("apply", { id: st.id, op });
      if (dead) return false;
      if (!r.ok) {
        showErr(r.err);
        return false;
      }
      await changed(r.j);
      return true;
    });
    chain = run.catch(() => false);
    return run;
  }

  function undo() {
    if (dead || delivering || st.id === null) return;
    chain = chain.then(async () => {
      if (dead) return;
      clearErr();
      const r = await call("undo", { id: st.id });
      if (dead) return;
      if (!r.ok) return showErr(r.err);
      if (r.j.undone) await changed(r.j);
      else status.textContent = tx("Нечего отменять", "Nothing to undo");
    });
  }

  function select(stmt, op, nested) {
    st.cur = { stmt, op, nested: nested.map((n) => [n[0], n[1]]) };
    tableCache.clear();
    refreshView();
    loadAvail();
  }

  function enter(alias) {
    select(st.cur.stmt, 0, st.cur.nested.concat([[st.cur.op, alias]]));
  }

  function leave() {
    const nested = st.cur.nested.slice();
    const last = nested.pop();
    select(st.cur.stmt, last ? last[0] : 0, nested);
  }

  const ctx = {
    view: () => st.view,
    avail: () => st.avail,
    cur: () => st.cur,
    q: () => ({ stmt: st.cur.stmt, nested: st.cur.nested.map((n) => [n[0], n[1]]) }),
    opi: () => st.cur.op,
    apply: applyOp,
    select,
    enter,
    fail,
    table(name) {
      const key = st.cur.stmt + "|" + name.toLowerCase();
      let p = tableCache.get(key);
      if (!p) {
        p = qget("children", { id: st.id, table: name, stmt: st.cur.stmt }).then((t) => (t && !t.error ? t : null), () => null);
        tableCache.set(key, p);
      }
      return p;
    },
  };

  const ta = el("textarea", {
    class: "xq-ta xb-ta", spellcheck: "false", wrap: "off", autocomplete: "off", autocapitalize: "off",
    "aria-label": tx("Текст запроса", "Query text"), placeholder: tx("ВЫБРАТЬ …", "SELECT …"),
  });
  const gut = el("pre", { class: "xq-gut", "aria-hidden": "true" }, "1");
  const hl = el("div", { class: "xq-hl", hidden: true });
  const ed = el("div", { class: "xq-ed xb-ed" }, gut, el("div", { class: "xq-tawrap" }, ta), hl);
  const edited = el("span", { class: "xb-edited dim", hidden: true }, tx("изменён, не применён", "changed, not applied"));
  const applyBtn = el("button", { class: "btn xb-btn xb-apply", type: "button", onclick: () => applyText() }, tx("Применить", "Apply"));
  const textRoot = el("div", { class: "xb-tab xb-text xb-text-tab" }, el("div", { class: "xb-textbar" }, applyBtn, edited), ed);

  function gutter() {
    const n = ta.value.split("\n").length;
    const lines = new Array(n);
    for (let i = 0; i < n; i++) lines[i] = i + 1;
    gut.textContent = lines.join("\n");
    gut.style.minWidth = String(String(n).length + 2) + "ch";
  }
  function placeHl() {
    const line = Number(hl.dataset.line || 0);
    if (line) hl.style.top = PAD + (line - 1) * LINE_H - ta.scrollTop + "px";
  }
  function clearMark() {
    hl.hidden = true;
    hl.dataset.line = "";
  }
  function mark(line, col) {
    const lines = ta.value.split("\n");
    if (line < 1 || line > lines.length) return;
    hl.hidden = false;
    hl.dataset.line = String(line);
    hl.dataset.col = String(col);
    placeHl();
    let off = 0;
    for (let i = 0; i < line - 1; i++) off += lines[i].length + 1;
    off += Math.max(0, Math.min(col - 1, lines[line - 1].length));
    ta.focus();
    ta.setSelectionRange(off, off);
    const top = (line - 1) * LINE_H;
    if (top < ta.scrollTop || top > ta.scrollTop + ta.clientHeight - 2 * LINE_H) ta.scrollTop = Math.max(0, top - 2 * LINE_H);
    placeHl();
  }
  ta.addEventListener("scroll", () => {
    gut.scrollTop = ta.scrollTop;
    placeHl();
  });
  ta.addEventListener("input", () => {
    gutter();
    clearMark();
    st.dirtyText = true;
    edited.hidden = false;
  });
  ta.addEventListener("keydown", (e) => {
    if (e.key === "Tab" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) {
      e.preventDefault();
      ta.setRangeText("\t", ta.selectionStart, ta.selectionEnd, "end");
      ta.dispatchEvent(new Event("input"));
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      applyText();
    }
  });

  const textTab = {
    root: textRoot,
    value: () => ta.value,
    setText(t) {
      ta.value = t;
      gutter();
      clearMark();
      edited.hidden = true;
    },
    render() {},
  };

  function applyText() {
    if (dead || delivering || st.id === null) return Promise.resolve(false);
    const run = chain.then(setTextNow);
    chain = run.catch(() => false);
    return run;
  }

  async function setTextNow() {
    if (dead) return false;
    clearErr();
    clearMark();
    const r = await call("set-text", { id: st.id, text: ta.value });
    if (dead) return false;
    if (!r.ok) {
      showErr(r.err, { head: tx("Ошибка в тексте запроса", "Error in the query text") });
      if (r.err.at) {
        strip.select(TEXT_TAB);
        mark(r.err.at[0], r.err.at[1]);
      }
      return false;
    }
    await changed(r.j);
    return true;
  }

  const parts = [
    [tx("Таблицы и поля", "Tables and fields"), createFieldsTab(ctx)],
    [tx("Группировка", "Grouping"), createGroupTab(ctx)],
    [tx("Условия", "Conditions"), createConditionsTab(ctx)],
    [tx("Дополнительно", "Advanced"), createModifiersTab(ctx)],
    [tx("Объединения/Псевдонимы", "Unions/Aliases"), createUnionsTab(ctx)],
    [tx("Порядок", "Order"), createOrderTab(ctx)],
    [tx("Итоги", "Totals"), createTotalsTab(ctx)],
    [tx("Построитель", "Builder"), createDcsTab(ctx)],
    [tx("Пакет запросов", "Query batch"), createBatchTab(ctx)],
    [tx("Запрос", "Query"), textTab],
    [tx("Связи", "Links"), createLinksTab(ctx)],
  ];
  let lastTab = 0;
  const tabs = parts.map((p) => p[1]);
  const strip = tabStrip(parts.map(([title, t]) => ({ title, node: t.root })), tx("Вкладки конструктора", "Builder tabs"), 0, (i) => {
    if (i < 9) lastTab = i;
    host.dataset.view = String(i);
    if (host.querySelector(".xb-back")) {
      host.querySelector(".xb-back").disabled = i === 0;
      host.querySelector(".xb-next").disabled = i >= 8;
    }
    if (!ready) return;
    if (dirty.has(i)) {
      dirty.delete(i);
      tabs[i].render();
    }
  });

  strip.root.querySelectorAll(".otab").forEach((button, i) => { button.hidden = i >= 9; button.title = parts[i][0]; });
  const where = el("div", { class: "xb-where" });
  host.addEventListener("xb-links", () => strip.select(10));
  function renderWhere() {
    const c = st.cur;
    const nested = c.nested.map((n) => n[1]);
    where.classList.toggle("xb-context-open", !!nested.length || !!init.literals);
    fill(where,
      el("span", { class: "xb-where-text" }, tx("Запрос ", "Query ") + (c.stmt + 1) + tx(", оператор ", ", operator ") + (c.op + 1) + (nested.length ? " › " + nested.join(" › ") : "")),
      nested.length ? el("button", { class: "btn xb-btn xb-leave", type: "button", onclick: leave }, tx("Выйти из подзапроса", "Leave the subquery")) : null,
      init.literals ? el("button", { class: "btn xb-btn xb-lits-btn", type: "button", onclick: () => toggleLiterals() }, tx("Из литерала модуля…", "From a module literal…")) : null);
  }

  function renderAll() {
    renderWhere();
    tabs.forEach((t, i) => {
      if (i === strip.index()) {
        dirty.delete(i);
        t.render();
      } else dirty.add(i);
    });
  }

  let lits = null;
  const litBox = el("div", { class: "xb-litbox", hidden: true });
  function toggleLiterals(force) {
    if (!init.literals) return;
    const show = force === undefined ? litBox.hidden : force;
    litBox.hidden = !show;
    if (show && !lits) {
      lits = createLiterals(init.literals, async (text) => {
        if (dead || delivering) return { chain: [tx("Передача запроса…", "Returning query…")], at: null };
        const e = await new Promise((resolve) => {
          chain = chain.then(async () => resolve(await openText(text)));
        });
        if (!e) {
          clearErr();
          litBox.hidden = true;
        }
        return e;
      });
      litBox.appendChild(lits.root);
    }
  }

  function deliveryLock(locked) {
    main.inert = locked;
    where.inert = locked;
    okBtn.disabled = locked || !canRun;
    if (runBtn) runBtn.disabled = locked;
    undoBtn.disabled = locked;
  }

  function deliver(run) {
    if (dead || delivering || !canRun) return;
    delivering = true;
    deliveryLock(true);
    const handover = chain.then(async (applied) => {
      if (dead || applied === false) return;
      if (st.dirtyText && !(await setTextNow())) return;
      if (dead) return;
      const detail = { text: st.text, run: !!run, target: init.origin === undefined ? null : init.origin, originText: init.originText, handled: false };
      window.dispatchEvent(new CustomEvent(OK_EVENT, { detail }));
      if (!detail.handled) return void showErr({ chain: [detail.error || tx("Консоль запросов недоступна", "The query console is not available")], at: null });
      closeWindow(win);
    });
    chain = handover.catch((e) => {
      if (!dead) showErr({ chain: [e.message], at: null });
      return false;
    }).finally(() => {
      delivering = false;
      if (!dead) deliveryLock(false);
    });
  }

  const okBtn = el("button", { class: "btn xb-btn xb-ok", type: "button", disabled: !canRun, title: canRun ? null : tx("Консоль работает только на базе 1CD: скопируйте текст", "The console works on a 1CD base only: copy the text"), onclick: () => deliver(false) }, tx("ОК", "OK"));
  const runBtn = canRun ? el("button", { class: "btn xb-btn xb-run", type: "button", onclick: () => deliver(true) }, tx("Выполнить", "Run")) : null;
  const cancelBtn = el("button", { class: "btn xb-btn xb-cancel", type: "button", onclick: () => closeWindow(win) }, tx("Отмена", "Cancel"));
  const undoBtn = el("button", { class: "btn xb-btn xb-undo", type: "button", onclick: () => undo() }, tx("Отменить действие", "Undo"));
  const copyBtn = el("button", { class: "btn xb-btn xb-copy", type: "button", onclick: () => {
    copyText(ta.value);
    copyBtn.dataset.copied = "1";
    status.textContent = tx("Текст скопирован", "Text copied");
  } }, tx("Копировать текст", "Copy text"));
  const backBtn = el("button", { class: "btn xb-btn xb-back", type: "button", disabled: true, onclick: () => strip.select(Math.max(0, strip.index() - 1)) }, tx("< Назад", "< Back"));
  const nextBtn = el("button", { class: "btn xb-btn xb-next", type: "button", onclick: () => strip.select(Math.min(8, strip.index() + 1)) }, tx("Далее >", "Next >"));
  const queryBtn = el("button", { class: "btn xb-btn xb-query-toggle", type: "button", onclick: () => strip.select(strip.index() === TEXT_TAB ? lastTab : TEXT_TAB) }, tx("Запрос", "Query"));
  const helpBtn = el("button", { class: "btn xb-btn xb-help", type: "button", onclick: () => openWindow({ kind: "helper", path: "Helper", title: tx("Синтакс-помощник", "Syntax helper") }) }, tx("Справка", "Help"));
  textTab.root.querySelector(".xb-textbar").append(undoBtn, copyBtn);
  const bottom = el("div", { class: "xb-bottom" }, queryBtn, status, el("span", { class: "xb-spacer" }), backBtn, nextBtn, runBtn, okBtn, cancelBtn, helpBtn);

  const main = el("div", { class: "xb-main" }, strip.root, litBox);
  fill(host, el("div", { class: "xb xb-root" }, where, errBox, main, bottom));

  (async () => {
    chain = chain.then(async () => {
      const wanted = init.text || "";
      const e = await openText(wanted);
      if (dead) return;
      if (e && !e.fatal) {
        const e2 = await openText("");
        if (e2 || dead) return e2 ? showErr(e2) : undefined;
        textTab.setText(wanted);
        st.dirtyText = true;
        edited.hidden = false;
        showErr(e, { head: tx("Ошибка в тексте запроса", "Error in the query text") });
        strip.select(TEXT_TAB);
        if (e.at) mark(e.at[0], e.at[1]);
      } else if (e) showErr(e);
      if (init.literals) toggleLiterals(true);
    });
    ready = true;
    renderAll();
  })();

  return {
    destroy() {
      dead = true;
      if (lits) lits.destroy();
      if (st.id !== null) qpost("close", { id: st.id }).catch(() => {});
    },
  };
}
