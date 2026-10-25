import { el, clear } from "../dom.js";
import { xget, xpost, isErrorOut } from "./net.js";
import { laterStrip, tx } from "./waiting.js";

const DELAY = 500;
const NUM_RE = /^-?\d+([.,]\d+)?$/;
const UUID_RE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

const quote = (s) => '"' + s.split('"').join('""') + '"';

function scalar(p, st, s) {
  const bad = (m) => ({ error: tx("параметр ", "parameter ") + p.name + ": " + m });
  switch (p.kind) {
    case "number":
      if (!NUM_RE.test(s.trim())) return bad(tx("ожидается число", "a number is expected"));
      return { repr: s.trim().replace(",", ".") };
    case "date": {
      if (!s) return bad(tx("укажите дату", "enter a date"));
      const v = s.length === 16 ? s + ":00" : s;
      return { repr: "'" + v + "'" };
    }
    case "bool":
      return { repr: s === "Ложь" ? "Ложь" : "Истина" };
    case "ref": {
      const t = s.trim();
      if (!t) return bad(tx("укажите значение", "enter a value"));
      if (t.includes("(")) return { repr: t };
      if (!UUID_RE.test(t)) return bad(tx("ожидается uuid", "a uuid is expected"));
      if (!st.refKind) return bad(tx("выберите вид ссылки", "choose the kind of the reference"));
      return { repr: st.refKind + "(" + t + ")" };
    }
    default:
      return { repr: quote(s) };
  }
}

export function reprOf(p, st) {
  if (st.isNull) return { repr: "NULL" };
  if (st.isUndef) return { repr: "Неопределено" };
  if (st.custom) {
    const t = (st.custom_text || "").trim();
    return t ? { repr: t } : { skip: true };
  }
  if (p.kind === "table") return { skip: true };
  if (st.arr) {
    const lines = (st.arr_text || "").split("\n").map((l) => l.replace(/\r$/, "")).filter((l) => l.length);
    const items = [];
    for (const l of lines) {
      const r = scalar(p, st, l);
      if (r.error) return r;
      items.push(r.repr);
    }
    return { repr: "Массив[" + items.join(", ") + "]" };
  }
  const v = st.v || "";
  if (!v && p.kind !== "bool") return { skip: true };
  return scalar(p, st, v);
}

function check(cb, label, onchange) {
  const input = el("input", { type: "checkbox", class: "xq-flag" });
  input.addEventListener("change", () => onchange(input.checked));
  return { input, node: el("label", { class: "xq-flaglbl" }, input, " ", label) };
}

export function createParams(opts) {
  let timer = null;
  let pending = false;
  let inflight = null;
  let seq = 0;
  let infos = [];
  let stateMap = new Map();
  const rows = el("div", { class: "xq-prows" });
  const note = el("div", { class: "xq-pnote2 dim" });
  const btn = el("button", { class: "btn", type: "button", onclick: () => refresh() }, tx("Параметры", "Parameters"));
  const root = el("div", { class: "xq-params" }, el("div", { class: "xq-phead" }, btn, note), rows);
  const choiceCache = new Map();

  function stOf(name) {
    if (!stateMap.has(name)) stateMap.set(name, {});
    return stateMap.get(name);
  }

  function changed() {
    if (opts.onChange) opts.onChange();
  }

  function choices(kind) {
    if (!choiceCache.has(kind)) {
      const p = xget("choices", { kind, limit: 50 }).catch(() => []);
      choiceCache.set(kind, p);
    }
    return choiceCache.get(kind);
  }

  function control(p, st) {
    const box = el("span", { class: "xq-pctl" });
    const set = (k, v) => {
      st[k] = v;
      changed();
    };
    if (st.custom) {
      const i = el("input", { type: "text", class: "xq-pin", "data-param": p.name, "aria-label": p.name, placeholder: tx("представление значения", "text of the value") });
      i.value = st.custom_text || "";
      i.addEventListener("input", () => set("custom_text", i.value));
      box.appendChild(i);
      return box;
    }
    if (p.kind === "table") {
      box.appendChild(laterStrip([tx("ждёт Q: параметр — таблица значений", "waits for Q: a value table parameter")]));
      return box;
    }
    if (st.arr) {
      const ta = el("textarea", { class: "xq-pin xq-parr", rows: 3, "data-param": p.name, "aria-label": p.name, placeholder: tx("по значению в строке", "one value per line") });
      ta.value = st.arr_text || "";
      ta.addEventListener("input", () => set("arr_text", ta.value));
      box.appendChild(ta);
      return box;
    }
    if (p.kind === "number" || p.kind === "string") {
      const i = el("input", { type: "text", class: "xq-pin", "data-param": p.name, "aria-label": p.name, inputmode: p.kind === "number" ? "decimal" : null });
      i.value = st.v || "";
      i.addEventListener("input", () => {
        set("v", i.value);
        const bad = p.kind === "number" && i.value.trim() !== "" && !NUM_RE.test(i.value.trim());
        i.classList.toggle("bad-input", bad);
        i.title = bad ? tx("ожидается число", "a number is expected") : "";
      });
      box.appendChild(i);
    } else if (p.kind === "date") {
      const i = el("input", { type: "datetime-local", step: "1", class: "xq-pin", "data-param": p.name, "aria-label": p.name });
      i.value = st.v || "";
      i.addEventListener("input", () => set("v", i.value));
      box.appendChild(i);
    } else if (p.kind === "bool") {
      const s = el("select", { class: "xq-pin", "data-param": p.name, "aria-label": p.name }, el("option", { value: "Истина" }, "Истина"), el("option", { value: "Ложь" }, "Ложь"));
      s.value = st.v || "Истина";
      s.addEventListener("change", () => set("v", s.value));
      box.appendChild(s);
    } else if (p.kind === "ref") {
      const kinds = p.refs || [];
      if (!st.refKind || (kinds.length && !kinds.includes(st.refKind))) st.refKind = kinds[0] || st.refKind || "";
      const ks = el("select", { class: "xq-pkind", "aria-label": tx("вид ссылки", "reference kind") }, kinds.map((k) => el("option", { value: k }, k)));
      if (!kinds.length) ks.appendChild(el("option", { value: "" }, "—"));
      ks.value = st.refKind || "";
      const pick = el("select", { class: "xq-pick", "aria-label": tx("значение", "value") }, el("option", { value: "" }, tx("— ввести uuid —", "— enter a uuid —")));
      const id = el("input", { type: "text", class: "xq-pin", "data-param": p.name, "aria-label": p.name, placeholder: "uuid" });
      id.value = (st.v || "").includes("(") ? "" : st.v || "";
      let choiceSeq = 0;
      const loadChoices = () => {
        const mineC = ++choiceSeq;
        clear(pick);
        pick.appendChild(el("option", { value: "" }, tx("— ввести uuid —", "— enter a uuid —")));
        if (!st.refKind) return;
        choices(st.refKind).then((list) => {
          if (mineC !== choiceSeq || !Array.isArray(list)) return;
          for (const c of list) pick.appendChild(el("option", { value: c.text }, c.text));
          pick.value = (st.v || "").includes("(") ? st.v : "";
        });
      };
      ks.addEventListener("change", () => {
        set("refKind", ks.value);
        set("v", "");
        id.value = "";
        loadChoices();
      });
      pick.addEventListener("change", () => {
        set("v", pick.value || id.value);
        if (pick.value) id.value = "";
      });
      id.addEventListener("input", () => {
        pick.value = "";
        set("v", id.value);
      });
      loadChoices();
      box.append(ks, " ", pick, " ", id);
    } else {
      st.custom = true;
      return control(p, st);
    }
    return box;
  }

  function render() {
    clear(rows);
    for (const p of infos) {
      const st = stOf(p.name);
      if (p.kind === "type" || p.kind === "any") {
        if (st.custom === undefined) st.custom = true;
      }
      const ctl = el("span", { class: "xq-pctlhost" });
      const redraw = () => {
        clear(ctl);
        ctl.appendChild(control(p, st));
      };
      redraw();
      const flags = el("span", { class: "xq-pflags" });
      const mk = (key, label, title) => {
        const c = check(null, label, (on) => {
          st[key] = on;
          if (key === "isNull" && on) st.isUndef = false;
          if (key === "isUndef" && on) st.isNull = false;
          sync();
          redraw();
          changed();
        });
        c.input.checked = !!st[key];
        if (title) c.node.title = title;
        c.input.dataset.flag = key;
        flags.appendChild(c.node);
        return c.input;
      };
      const inputs = [
        mk("isNull", "NULL"),
        mk("isUndef", "Неопределено"),
        mk("custom", tx("своё представление", "own representation"), tx("текст значения как есть", "the text of the value as it is")),
        p.kind === "table" ? null : mk("arr", tx("массив", "array"), tx("значения через перевод строки", "values separated by newlines")),
      ];
      const sync = () => {
        for (const i of inputs) if (i) i.checked = !!st[i.dataset.flag];
      };
      rows.appendChild(
        el("div", { class: "xq-prow", "data-param": p.name },
          el("span", { class: "xq-pname" }, "&" + p.name),
          el("span", { class: "xq-pty dim", title: p.ty }, p.ty),
          ctl,
          flags),
      );
    }
    root.hidden = false;
    root.classList.toggle("empty", infos.length === 0);
  }

  function refresh() {
    clearTimeout(timer);
    pending = false;
    const p = doRefresh();
    inflight = p;
    return p;
  }

  function doRefresh() {
    const mine = ++seq;
    const text = opts.getText();
    if (!text.trim()) {
      infos = [];
      note.textContent = "";
      render();
      return Promise.resolve();
    }
    return xpost("parameters", { text }).then(
      (a) => {
        if (mine !== seq) return;
        if (isErrorOut(a)) {
          infos = [];
          note.textContent = tx("Параметры не определены: ", "Parameters are not found: ") + (a.chain[0] || "").split("\n")[0];
          note.classList.toggle("xq-later-note", a.phase === "later");
          render();
          return;
        }
        infos = Array.isArray(a) ? a : [];
        note.textContent = infos.length ? "" : tx("Параметров нет", "No parameters");
        note.classList.remove("xq-later-note");
        render();
      },
      (e) => {
        if (mine === seq) note.textContent = tx("Ошибка: ", "Error: ") + e.message;
      },
    );
  }

  function schedule() {
    clearTimeout(timer);
    pending = true;
    timer = setTimeout(refresh, DELAY);
  }

  return {
    root, schedule, refresh,
    flush() {
      if (pending) return refresh();
      return inflight || Promise.resolve();
    },
    values() {
      const params = [];
      const errors = [];
      for (const p of infos) {
        const r = reprOf(p, stOf(p.name));
        if (r.error) errors.push(r.error);
        else if (r.repr !== undefined) params.push({ name: p.name, repr: r.repr });
      }
      return { params, errors };
    },
    state() {
      const o = {};
      for (const p of infos) o[p.name] = Object.assign({}, stOf(p.name));
      return o;
    },
    setState(map) {
      stateMap = new Map();
      for (const k of Object.keys(map || {})) stateMap.set(k, Object.assign({}, map[k]));
    },
    destroy() {
      clearTimeout(timer);
      pending = false;
      seq++;
    },
  };
}
