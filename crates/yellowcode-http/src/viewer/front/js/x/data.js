import { el, fill, clear } from "../dom.js";
import { openWindow, update } from "../tabs.js";
import { createDataGrid, versionLast } from "./data_grid.js";
import { openCard, openRecord } from "./data_card.js";
import { xget } from "./net.js";
import { tx } from "./waiting.js";

export const DATA_KIND = "x-data";
export const DATA_ITEM_KEY = "x-open-data";
const STALE_RESTARTS = 3;
const DATA_KINDS = new Set([
  "Catalog", "Document", "DocumentJournal", "Enum", "Constant", "ExchangePlan", "ChartOfCharacteristicTypes",
  "ChartOfAccounts", "ChartOfCalculationTypes", "InformationRegister", "AccumulationRegister",
  "AccountingRegister", "CalculationRegister", "BusinessProcess", "Task",
]);
const HINT = {
  number: () => "123.5",
  date: () => "2024-01-31T23:59:59",
  string: () => tx("текст как есть", "text as is"),
  ref: (ty) => {
    const m = /([A-Za-z]+Ref\.[^,()\s]+)/.exec(ty || "");
    return (m ? m[1] : "CatalogRef.X") + "(<uuid>) " + tx("или", "or") + " <uuid>";
  },
};

const dataTitle = (path) => tx("Данные: ", "Data: ") + path;
const severalText = (field) => tx("в базе несколько значений ", "the base has several values of ") + field + tx(", показано одно", ", one is shown");
const areaText = () => tx("Область данных", "Data area");
const notBaseText = () => tx("Данные открываются на базе: откройте файл 1Cv8.1CD", "Data opens on a base: open a 1Cv8.1CD file");

function dget(route, params, signal) {
  const q = [];
  for (const k of Object.keys(params)) {
    const v = params[k];
    if (v !== null && v !== undefined) q.push(encodeURIComponent(k) + "=" + encodeURIComponent(v));
  }
  const url = "/api/x/data/" + route + (q.length ? "?" + q.join("&") : "");
  return fetch(url, signal ? { signal } : undefined).then((r) =>
    r.json().catch(() => null).then((j) => {
      if (r.ok && j) return j;
      const e = new Error((j && j.error) || String(r.status));
      e.status = r.status;
      if (j && j.availableIndexes) e.availableIndexes = j.availableIndexes;
      throw e;
    }),
  );
}

let sourceKind = null;
let probing = false;
let tables = null;
let reading = null;

function probeSource() {
  if (sourceKind || probing) return;
  probing = true;
  dget("source", {}).then(
    (s) => {
      probing = false;
      sourceKind = s;
    },
    () => {
      probing = false;
    },
  );
}

function readTables() {
  if (tables) return Promise.resolve(tables);
  if (!reading) {
    reading = xget("info")
      .then((info) => {
        if (info.enabled === false) throw new Error(notBaseText());
        if (info.error) throw new Error(tx("База не открыта: ", "The base is not open: ") + info.error);
        return dget("tables", {});
      })
      .then((list) => {
        tables = new Set(list.map((t) => t.path));
        return tables;
      });
    reading.then(
      () => {
        reading = null;
      },
      () => {
        reading = null;
      },
    );
  }
  return reading;
}

let toastNode = null;
let toastTimer = 0;

function toast(text) {
  clearTimeout(toastTimer);
  if (toastNode) toastNode.remove();
  toastNode = el("div", { class: "xd-toast", role: "alert", onclick: () => toastNode && toastNode.remove() }, text);
  document.body.appendChild(toastNode);
  toastTimer = setTimeout(() => toastNode && toastNode.remove(), 8000);
}

function openWhenKnown(path) {
  readTables().then(
    (t) => {
      if (t.has(path)) openData(path);
      else toast(tx("Нет хранимой таблицы данных: ", "No stored data table: ") + path);
    },
    (e) => toast(e.message),
  );
}

probeSource();

export function dataTreeItems(path) {
  const kind = path ? path.split(".") : [];
  if (kind.length !== 2 || !DATA_KINDS.has(kind[0])) return [];
  probeSource();
  const label = tx("Открыть данные", "Open data");
  const entry = (more) => [{ sep: true }, Object.assign({ key: DATA_ITEM_KEY, label, title: dataTitle(path) }, more)];
  if (sourceKind && !sourceKind.base) return entry({ disabled: true, title: sourceKind.reason || notBaseText() });
  if (tables) return tables.has(path) ? entry({ onclick: () => openData(path) }) : [];
  readTables().catch(() => {});
  return entry({ onclick: () => openWhenKnown(path) });
}

export function openData(path) {
  return openWindow({ kind: DATA_KIND, path, title: dataTitle(path) });
}

export function createDataTab(win) {
  const host = el("div", { class: "xd-host" }, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
  win.pane.appendChild(host);
  update(win, { title: dataTitle(win.path) });
  let view = null;
  let dead = false;
  dget("indexes", { path: win.path }).then(
    (info) => {
      if (!dead) view = buildView(win.path, host, info);
    },
    (e) => {
      if (dead) return;
      const text = e.status === 409 ? e.message || notBaseText() : tx("Ошибка: ", "Error: ") + e.message;
      fill(host, el("div", { class: "xq-notbase" }, el("div", { class: "msg bad xd-fail", "data-status": e.status || null }, text)));
    },
  );
  return {
    destroy() {
      dead = true;
      if (view) view.destroy();
    },
  };
}

function buildView(path, host, info) {
  const indexes = info.indexes || [];
  const byName = new Map(indexes.map((ix) => [ix.name, ix]));
  const period = info.period || null;
  let index = info.default || null;
  let order = "asc";
  let applied = { eq: [], range: {} };
  let grid = null;
  let stale = 0;
  let condRows = [];
  const kinds = new Map();
  const types = new Map();
  let columns = [];
  let periodSet = false;

  const orderLabel = el("span", { class: "xd-order" });
  const indexSel = el("select", { class: "xd-index", "aria-label": tx("Индекс", "Index") });
  if (!info.default) indexSel.appendChild(el("option", { value: "" }, tx("порядок хранения", "storage order")));
  for (const ix of indexes) {
    indexSel.appendChild(el("option", { value: ix.name }, ix.name + " (" + ix.fields.map((f) => f.name).join(", ") + ")"));
  }
  const descOpt = el("option", { value: "desc" });
  const dirSel = el("select", { class: "xd-dir", "aria-label": tx("Направление", "Direction") },
    el("option", { value: "asc" }, tx("По возрастанию", "Ascending")), descOpt);
  const bar = el("div", { class: "xq-bar xd-bar" }, orderLabel, el("label", { class: "xd-lbl" }, tx("Индекс: ", "Index: "), indexSel), dirSel);

  const from = el("input", { class: "xd-from", type: "date", "aria-label": tx("Период с", "Period from") });
  const to = el("input", { class: "xd-to", type: "date", "aria-label": tx("Период по", "Period to") });
  const pfxFields = period ? ((byName.get(period.index) || {}).fields || []).slice(0, period.prefix.length) : [];
  const pfxShown = pfxFields.map((f) => period.several.includes(f.name));
  const pfxInputs = pfxFields.map((f, i) => {
    const inp = el("input", { class: "xd-pfx", type: "text", "data-field": f.name, value: period.prefix[i], "aria-label": areaText() + " " + f.name, spellcheck: "false", autocomplete: "off" });
    inp.addEventListener("keydown", onEnter);
    return inp;
  });
  const pfxNames = pfxFields.map((f) => f.name);
  const clearPeriodBtn = el("button", { class: "btn xd-pclear", type: "button", title: tx("Очистить период", "Clear period"),
    "aria-label": tx("Очистить период", "Clear period"), onclick: () => clearPeriod() }, "×");
  const periodBox = el("div", { class: "xd-period" },
    el("div", { class: "xd-prow" }, tx("Период: с ", "Period: from "), from, tx(" по ", " to "), to, clearPeriodBtn),
    pfxFields.map((f, i) => el("div", { class: "xd-prow xd-pfxrow", "data-field": f.name, hidden: !pfxShown[i] },
      el("span", { class: "xd-fname", title: f.name }, areaText() + ":"), pfxInputs[i],
      el("span", { class: "xd-several" }, severalText(f.name)))),
    period ? period.several.filter((f) => !pfxNames.includes(f)).map((f) => el("span", { class: "xd-several" }, severalText(f))) : null);
  if (period) {
    periodBox.title = [tx("По индексу ", "By the index ") + period.index + ", " + tx("поле ", "field ") + period.field,
      ...pfxFields.flatMap((f, i) => (pfxShown[i] ? [] : [areaText() + " " + f.name + " = " + period.prefix[i]]))].join("\n");
  } else {
    from.disabled = true;
    to.disabled = true;
    clearPeriodBtn.disabled = true;
    periodBox.classList.add("off");
    periodBox.title = tx("Отбор по периоду недоступен: ", "No selection by period: ") + (info.periodReason || "");
  }
  for (const i of [from, to]) i.addEventListener("keydown", onEnter);

  const has = info.filters || {};
  const flag = (cls, label, options) => {
    const sel = el("select", { class: cls, "aria-label": label },
      options.map(([v, ru, en]) => el("option", { value: v }, tx(ru, en))));
    sel.addEventListener("change", () => restart(null));
    return sel;
  };
  const postedSel = has.posted ? flag("xd-posted", tx("Проведён", "Posted"),
    [["", "все", "all"], ["true", "проведённые", "posted"], ["false", "непроведённые", "not posted"]]) : null;
  const markSel = has.deletionMark ? flag("xd-mark", tx("Пометка удаления", "Deletion mark"),
    [["", "все", "all"], ["false", "без помеченных", "not marked"], ["true", "только помеченные", "marked only"]]) : null;
  const flags = postedSel || markSel ? el("div", { class: "xd-flags" },
    postedSel ? el("label", { class: "xd-lbl" }, tx("Проведён: ", "Posted: "), postedSel) : null,
    markSel ? el("label", { class: "xd-lbl" }, tx("Пометка удаления: ", "Deletion mark: "), markSel) : null) : null;

  const conds = el("div", { class: "xd-conds" });
  const applyBtn = el("button", { class: "btn xd-apply", type: "button", onclick: () => apply() }, tx("Применить", "Apply"));
  const resetBtn = el("button", { class: "btn xd-reset", type: "button", onclick: () => reset() }, tx("Очистить отбор", "Clear selection"));
  const filter = el("div", { class: "xd-filter" }, flags, periodBox, conds, el("div", { class: "xd-actions" }, applyBtn, resetBtn));
  const note = el("div", { class: "xd-note", role: "status", hidden: true });
  const gridHost = el("div", { class: "xd-gridhost" });
  fill(host, el("div", { class: "xd" }, bar, filter, note, gridHost));

  function onEnter(ev) {
    if (ev.key === "Enter") {
      ev.preventDefault();
      apply();
    }
  }

  function input(cls, label) {
    const i = el("input", { class: cls, type: "text", "aria-label": label, spellcheck: "false", autocomplete: "off" });
    i.addEventListener("input", refreshConds);
    i.addEventListener("keydown", onEnter);
    return i;
  }

  function buildConds() {
    clear(conds);
    condRows = [];
    const ix = index ? byName.get(index) : null;
    if (!ix) {
      conds.appendChild(el("div", { class: "xq-wait" }, tx("Условия задаются по индексу", "Conditions need an index")));
      return;
    }
    ix.fields.forEach((f, i) => {
      const eq = input("xd-eq", f.name + tx(" равно", " equals"));
      const gte = input("xd-gte", f.name + tx(" от", " from"));
      const lte = input("xd-lte", f.name + tx(" до", " to"));
      const byPeriod = !!period && index === period.index && i <= period.prefix.length;
      const area = byPeriod && i < period.prefix.length;
      const mark = area && period.several.includes(f.name) ? el("span", { class: "xd-several" }, severalText(f.name)) : null;
      const row = el("div", { class: "xd-cond" + (byPeriod ? " xd-byperiod" : ""), "data-field": f.name, hidden: area && !period.several.includes(f.name) },
        el("span", { class: "xd-fname" }, f.name), " = ", eq, tx(" от ", " from "), gte, tx(" до ", " to "), lte, mark);
      conds.appendChild(row);
      condRows.push({ f, row, eq, gte, lte, byPeriod });
    });
    refreshConds();
    applyKinds();
  }

  function refreshConds() {
    let blocked = null;
    for (const r of condRows) {
      const set = (eqOn, rangeOn, title) => {
        r.eq.disabled = !eqOn;
        r.gte.disabled = !rangeOn;
        r.lte.disabled = !rangeOn;
        r.row.classList.toggle("off", !eqOn);
        if (title) r.row.title = title;
        else r.row.removeAttribute("title");
      };
      if (r.byPeriod) {
        set(false, false, tx("Задаётся в «Период»", "Set in «Period»"));
        blocked = blocked || tx("Условия после поля ", "No conditions after the field ") + period.field +
          tx(" не задаются: дата задаётся в «Период»", ": the date is set in «Period»");
      } else if (!r.f.conditions) {
        set(false, false, r.f.reason || tx("условия по полю не поддерживаются", "the field takes no conditions"));
        blocked = blocked || tx("Условия после поля ", "No conditions after the field ") + r.f.name + tx(" не задаются", "");
      } else if (blocked) {
        set(false, false, blocked);
      } else if (r.eq.value.trim() !== "") {
        set(true, false, "");
      } else {
        set(true, true, "");
        blocked = tx("Сначала задайте «равно» для поля ", "First set «equals» of the field ") + r.f.name;
      }
    }
  }

  function applyKinds() {
    const hint = (name) => {
      const h = HINT[kinds.get(name)];
      return h ? h(types.get(name)) : null;
    };
    for (const r of condRows) {
      const h = hint(r.f.name);
      if (h) for (const i of [r.eq, r.gte, r.lte]) i.placeholder = h;
    }
    pfxFields.forEach((f, i) => {
      const h = hint(f.name);
      if (h) pfxInputs[i].placeholder = h;
    });
  }

  function value(r, i) {
    if ((i.disabled && !r.byPeriod) || i.value.trim() === "") return null;
    return kinds.get(r.f.name) === "string" ? i.value : i.value.trim();
  }

  function collect() {
    const eq = [];
    const range = {};
    for (const r of condRows) {
      const v = value(r, r.eq);
      if (v !== null) {
        eq.push(v);
        continue;
      }
      const a = value(r, r.gte);
      const b = value(r, r.lte);
      if (a !== null) range.gte = a;
      if (b !== null) range.lte = b;
      break;
    }
    return { eq, range };
  }

  function applyPeriod() {
    if (index !== period.index) setIndex(period.index, false);
    period.prefix.forEach((v, i) => {
      const r = condRows[i];
      const own = pfxInputs[i] ? pfxInputs[i].value : "";
      if (pfxInputs[i] && own.trim() === "") pfxInputs[i].value = v;
      if (r) r.eq.value = own.trim() === "" ? v : own;
    });
    const d = condRows[period.prefix.length];
    if (d) {
      d.eq.value = "";
      d.gte.value = from.value || "";
      d.lte.value = to.value ? to.value + "T23:59:59" : "";
    }
    periodSet = true;
    refreshConds();
  }

  function dropPeriod() {
    periodSet = false;
    for (const r of condRows) {
      if (!r.byPeriod) continue;
      r.eq.value = "";
      r.gte.value = "";
      r.lte.value = "";
    }
    refreshConds();
  }

  function clearPeriod() {
    from.value = "";
    to.value = "";
    apply();
  }

  function apply() {
    if (period && (from.value || to.value)) applyPeriod();
    else if (periodSet) dropPeriod();
    applied = collect();
    restart(null);
  }

  function reset() {
    if (postedSel) postedSel.value = "";
    if (markSel) markSel.value = "";
    from.value = "";
    to.value = "";
    periodSet = false;
    pfxInputs.forEach((inp, i) => {
      inp.value = period.prefix[i];
    });
    for (const r of condRows) {
      r.eq.value = "";
      r.gte.value = "";
      r.lte.value = "";
    }
    refreshConds();
    applied = { eq: [], range: {} };
    restart(null);
  }

  function refreshDir() {
    descOpt.textContent = period && index === period.index ? tx("Сначала новые", "Newest first") : tx("По убыванию", "Descending");
    dirSel.disabled = !index;
    if (!index) {
      order = "asc";
      dirSel.value = "asc";
      dirSel.title = tx("Направление задаётся только по индексу", "The direction needs an index");
    } else dirSel.removeAttribute("title");
  }

  function setIndex(name, run) {
    index = name || null;
    indexSel.value = index || "";
    applied = { eq: [], range: {} };
    buildConds();
    refreshDir();
    if (run) restart(null);
  }

  function labelOrder(ix) {
    orderLabel.textContent = tx("Порядок: ", "Order: ") + (ix ? tx("индекс ", "index ") + ix : tx("порядок хранения", "storage order"));
  }

  function restart(staleError) {
    if (grid) grid.destroy();
    grid = null;
    if (staleError) stale++;
    else {
      stale = 0;
      note.hidden = true;
    }
    if (stale > STALE_RESTARTS) {
      note.hidden = false;
      note.textContent = tx("Листание остановлено: ", "The list stopped: ") + staleError.message;
      clear(gridHost);
      return;
    }
    if (staleError) {
      note.hidden = false;
      note.textContent = tx("Данные изменились, листание начато заново", "The data changed, the list started again");
    }
    labelOrder(index);
    const ask = { path, index: index || undefined, order: index ? order : undefined };
    if (applied.eq.length) ask.eq = JSON.stringify(applied.eq);
    if (Object.keys(applied.range).length) ask.range = JSON.stringify(applied.range);
    if (postedSel && postedSel.value) ask.posted = postedSel.value;
    if (markSel && markSel.value) ask.deletionMark = markSel.value;
    grid = createDataGrid({
      filtered: !!(ask.posted || ask.deletionMark),
      fetch: (cursor, signal) => dget("page", Object.assign({}, ask, { cursor: cursor || undefined }), signal).then((p) => {
        versionLast(p.columns || [], p.rows || [], p.cut);
        return p;
      }),
      onPage(p, asked) {
        labelOrder(p.index);
        if (asked !== null) stale = 0;
        if (!kinds.size && p.columns) {
          columns = p.columns;
          for (const c of p.columns) {
            kinds.set(c.name, c.kind);
            types.set(c.name, c.ty);
          }
          applyKinds();
        }
      },
      onStale: (e) => restart(e),
      onRowActivate: activateRow,
      onRefActivate: openCard,
    });
    fill(gridHost, grid.root);
  }

  function activateRow(i, row) {
    const at = columns.findIndex((c) => c.name === "Ссылка" && c.kind === "ref");
    if (at >= 0 && row.refs[at]) openCard(row.refs[at]);
    else openRecord(path, i, columns, row);
  }

  indexSel.addEventListener("change", () => {
    from.value = "";
    to.value = "";
    periodSet = false;
    setIndex(indexSel.value, true);
  });
  dirSel.addEventListener("change", () => {
    order = dirSel.value;
    restart(null);
  });

  indexSel.value = index || "";
  buildConds();
  refreshDir();
  restart(null);
  return {
    destroy() {
      if (grid) grid.destroy();
    },
  };
}
