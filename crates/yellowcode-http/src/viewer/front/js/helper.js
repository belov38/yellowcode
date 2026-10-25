import { el, fill, portions } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { registerKind } from "./tabs.js";

const LIMIT = 100;
const ROWS = 200;
const DELAY = 250;

export const HELPER_PATH = "Helper";

const kindText = (k) =>
  ({ type: T.helperTypeKind, method: T.helperMethodKind, property: T.helperPropKind, global: T.helperGlobalKind }[k] || k);

function availNode(list) {
  return el("span", { class: "avail dim" }, (list || []).join(", "));
}

function membersSection(title, items, make, mark) {
  const body = el("div", { class: "hmembers" });
  const foot = el("div", { class: "fmore" });
  portions(body, foot, items, make, ROWS, (n) => T.moreRows + n);
  const sec = el("details", { class: "dsec", open: "" }, el("summary", null, title + " (" + items.length + ")"), body, foot);
  sec.dataset.mark = mark || "";
  return sec;
}

function cardNode(t, focus) {
  const card = el("div", { class: "hcard", "data-type": t.name });
  card.appendChild(el("div", { class: "wtitle" }, el("span", { class: "kind" }, t.kind),
    el("span", { class: "name" }, t.name), " ", el("span", { class: "dim" }, t.name_other)));
  if ((t.avail || []).length) card.appendChild(el("div", null, el("strong", null, T.helperAvail + ": "), availNode(t.avail)));
  const isFocus = (name) => !!focus && focus.toLowerCase() === String(name).toLowerCase();
  const hl = (name) => (isFocus(name) ? " hl" : "");
  const focusFirst = (list) => (focus ? list.filter((x) => isFocus(x.name)).concat(list.filter((x) => !isFocus(x.name))) : list);
  const props = focusFirst(t.properties || []);
  if (props.length) {
    card.appendChild(membersSection(T.helperPropsH, props, (p) =>
      el("div", { class: "hprop" + hl(p.name), "data-name": p.name },
        el("span", { class: "hname" }, p.name), " ", el("span", { class: "dim" }, p.name_other), " ",
        el("span", { class: "dim" }, [p.readable ? T.helperRead : null, p.writable ? T.helperWrite : null].filter(Boolean).join(", ")), " ",
        availNode(p.avail)), "properties"));
  }
  const methods = focusFirst(t.methods || []);
  if (methods.length) {
    card.appendChild(membersSection(T.helperMethodsH, methods, (m) =>
      el("div", { class: "hmethod" + hl(m.name), "data-name": m.name },
        (m.signatures || []).map((s) => el("div", { class: "hsig" }, s)),
        el("div", { class: "dim" }, el("span", null, m.name_other), m.returns ? " · " + T.helperReturns : "", " ", availNode(m.avail))), "methods"));
  }
  const values = t.values || [];
  if (values.length) {
    card.appendChild(membersSection(T.helperValuesH, values, (v) =>
      el("div", { class: "hvalue" + hl(v.name), "data-name": v.name }, el("span", { class: "hname" }, v.name), " ", el("span", { class: "dim" }, v.name_other)), "values"));
  }
  return card;
}

function createHelper(win) {
  let searchSeq = 0;
  let cardSeq = 0;
  let timer = null;
  const input = el("input", { type: "search", class: "hsearch", placeholder: T.helperSearch, "aria-label": T.helperSearch, autocomplete: "off", spellcheck: "false" });
  const count = el("span", { class: "msg hcount" });
  const results = el("ul", { class: "hresults", role: "listbox", "aria-label": T.helperItem });
  const card = el("div", { class: "hcardhost" }, el("div", { class: "msg" }, T.helperHint));
  win.pane.append(el("div", { class: "helperwin" },
    el("div", { class: "hbar" }, input, count),
    el("div", { class: "hbody" }, results, card)));
  win.pane.dataset.helper = "1";

  function open(typeName, focus) {
    const mine = ++cardSeq;
    fill(card, el("div", { class: "msg" }, T.loading));
    api("helper/type", { name: typeName }).then(
      (t) => { if (mine === cardSeq && !win.closed) { fill(card, cardNode(t, focus)); const h = card.querySelector(".hl"); if (h && h.scrollIntoView) h.scrollIntoView({ block: "nearest" }); } },
      (e) => { if (mine === cardSeq && !win.closed) fill(card, el("div", { class: "msg bad" }, T.error + e.message)); },
    );
  }

  function hitNode(h) {
    const li = el("li", { class: "hhit", role: "option", tabindex: "0", "data-kind": h.kind, "data-name": h.name, "data-owner": h.owner || null },
      el("span", { class: "hk dim" }, kindText(h.kind)), " ", el("span", { class: "hname" }, h.name),
      h.owner ? el("span", { class: "dim" }, " · " + T.helperOwner + h.owner) : null);
    const act = () => {
      for (const x of results.querySelectorAll(".sel")) x.classList.remove("sel");
      li.classList.add("sel");
      if (h.kind === "type") open(h.name, null);
      else if (h.owner) open(h.owner, h.name);
      else fill(card, el("div", { class: "hcard" }, el("div", { class: "wtitle" }, el("span", { class: "kind" }, kindText(h.kind)), el("span", { class: "name" }, h.name)),
        el("div", { class: "msg" }, T.helperGlobal)));
    };
    li.addEventListener("click", act);
    li.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); act(); } });
    return li;
  }

  function search() {
    const q = input.value.trim();
    const mine = ++searchSeq;
    if (!q) {
      fill(results);
      count.textContent = "";
      return;
    }
    api("helper", { q, limit: LIMIT }).then(
      (d) => {
        if (mine !== searchSeq || win.closed) return;
        const hits = d.hits || [];
        fill(results, hits.map(hitNode));
        count.textContent = hits.length ? T.helperFound + d.total + (d.total > hits.length ? T.shown + hits.length : "") : T.nothing;
      },
      (e) => { if (mine === searchSeq && !win.closed) count.textContent = T.error + e.message; },
    );
  }
  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(search, DELAY);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { clearTimeout(timer); search(); }
  });
  return { destroy() { clearTimeout(timer); } };
}

export function initHelper() {
  registerKind("helper", { create: createHelper });
}
