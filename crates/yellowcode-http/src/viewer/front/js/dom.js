export function el(tag, attrs, ...children) {
  const e = document.createElement(tag);
  if (attrs) {
    for (const k of Object.keys(attrs)) {
      const v = attrs[k];
      if (v === null || v === undefined) continue;
      if (k === "class") e.className = v;
      else if (k.startsWith("on")) {
        if (typeof v === "function") e.addEventListener(k.slice(2), v);
      }
      else if (k === "hidden" || k === "disabled" || k === "checked" || k === "value") e[k] = v;
      else e.setAttribute(k, String(v));
    }
  }
  append(e, children);
  return e;
}

export function append(parent, children) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (Array.isArray(c)) append(parent, c);
    else if (c instanceof Node) parent.appendChild(c);
    else parent.appendChild(document.createTextNode(String(c)));
  }
}

export function clear(e) {
  while (e.firstChild) e.removeChild(e.firstChild);
}

export function $(id) {
  return document.getElementById(id);
}

export function fill(e, ...children) {
  clear(e);
  append(e, children);
  return e;
}

export function portions(parent, foot, items, make, step, label) {
  let list = items;
  let shown = 0;
  const next = () => {
    const end = Math.min(shown + step, list.length);
    const frag = document.createDocumentFragment();
    for (let i = shown; i < end; i++) frag.appendChild(make(list[i], i));
    shown = end;
    parent.appendChild(frag);
    clear(foot);
    if (shown < list.length) {
      foot.appendChild(el("button", { class: "btn more-btn", type: "button", onclick: next }, label(list.length - shown)));
    }
  };
  next();
  return {
    reset(newItems) {
      list = newItems;
      shown = 0;
      clear(parent);
      next();
    },
  };
}
