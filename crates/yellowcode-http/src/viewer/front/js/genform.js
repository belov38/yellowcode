import { el, fill } from "./dom.js";
import { api } from "./api.js";
import { T } from "./i18n.js";
import { openWindow, registerKind } from "./tabs.js";

const SIZE = [1024, 768];

export function genFormButton(path) {
  const btn = el("button", {
    class: "btn open-genform", type: "button", hidden: true,
    onclick: () => openWindow({ kind: "genform", path, title: T.genForm, icon: "sub:Form" }),
  }, T.genForm);
  api("form/roles", { path }).then(
    (d) => { btn.hidden = !((d.roles || []).length); },
    () => {},
  );
  return btn;
}

function createGenForm(win) {
  const sel = el("select", { class: "gen-role", "aria-label": T.genRole });
  const host = el("div", { class: "form-host" });
  const node = el("div", { class: "fpreview", "data-tab": "generated" }, el("div", { class: "formbar" }, sel), host);
  win.pane.appendChild(node);
  const show = () => {
    const [w, h] = SIZE;
    const fr = el("iframe", { sandbox: "allow-same-origin", title: win.title });
    fr.src = "/form?path=" + encodeURIComponent(win.path) + "&generated=" + encodeURIComponent(sel.value) +
      "&w=" + w + "&h=" + h;
    fr.style.width = w + "px";
    fr.style.height = h - 189 + 36 + "px";
    fill(host, fr);
  };
  sel.addEventListener("change", show);
  api("form/roles", { path: win.path }).then(
    (d) => {
      if (win.closed) return;
      const roles = d.roles || [];
      if (!roles.length) {
        fill(host, el("div", { class: "msg" }, T.none));
        return;
      }
      for (const r of roles) sel.appendChild(el("option", { value: r }, r));
      show();
    },
    (e) => { if (!win.closed) fill(host, el("div", { class: "msg bad" }, T.error + e.message)); },
  );
  return {};
}

export function initGenForm() {
  registerKind("genform", { create: createGenForm });
}
