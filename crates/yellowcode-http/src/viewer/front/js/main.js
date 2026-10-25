import { $, el, fill } from "./dom.js";
import { api, setIconVersion } from "./api.js";
import { T, setLang, plural } from "./i18n.js";
import { initTabs, onActive, openFromHash, parseHash } from "./tabs.js";
import { initTree, selectInTree } from "./tree.js";
import { initProps, setPaletteTarget } from "./props.js";
import { initModule } from "./module.js";
import { initForm } from "./form.js";
import { initGenForm } from "./genform.js";
import { initTemplate } from "./template.js";
import { initRights } from "./rights.js";
import { initHelper } from "./helper.js";
import { initSearch } from "./search.js";
import { retitleMenus } from "./menu.js";
import { initCommands, refreshCommands } from "./commands.js";
import { initSkin } from "./skin.js";
import { initDesktop } from "./desktop.js";
import "./x/index.js";

function showError(e) {
  fill($("palette-body"), el("div", { class: "msg bad" }, T.error + (e.message || String(e))));
}

function applyLang() {
  $("search").placeholder = T.search;
  $("search").setAttribute("aria-label", T.search);
  $("lbl-names").textContent = T.names;
  $("lbl-regex").textContent = T.regex;
  $("tree").setAttribute("aria-label", T.tree);
  $("palette").setAttribute("aria-label", T.palette);
  $("empty").textContent = T.pick;
  $("banner-x").setAttribute("aria-label", T.dismiss);
  $("lbl-alpha").textContent = T.byAlphabet;
  retitleMenus();
  refreshCommands();
}

function header(info) {
  document.title = info.name;
  $("cfgname").textContent = info.name;
  $("syn").textContent = info.synonym || "";
  const s = info.stats || {};
  let total = 0;
  for (const k of Object.keys(s.objects || {})) total += s.objects[k];
  $("stats").textContent = [
    plural(total, "objects"), plural(s.modules || 0, "modulesN"), plural(s.module_lines || 0, "linesN"),
    plural(s.forms || 0, "formsN"), plural(s.templates || 0, "templatesN"),
  ].join(", ");
}

function buildMenus() {
  initDesktop();
  initCommands(showError);
}

function splitter(id, prop, fromRight) {
  const sp = $(id);
  let dragging = false;
  sp.addEventListener("pointerdown", (e) => {
    dragging = true;
    sp.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  sp.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const x = fromRight ? window.innerWidth - e.clientX : e.clientX;
    $("layout").style.setProperty(prop, Math.max(160, Math.min(x, window.innerWidth - 400)) + "px");
  });
  const stop = () => {
    dragging = false;
  };
  sp.addEventListener("pointerup", stop);
  sp.addEventListener("pointercancel", stop);
}

function start() {
  buildMenus();
  initSkin(showError);
  initTabs();
  initProps();
  initModule();
  initForm();
  initGenForm();
  initTemplate();
  initRights();
  initHelper();
  initSearch();
  splitter("splitter", "--side", false);
  splitter("splitter-pal", "--pal", true);
  onActive((win) => {
    const path = win && win.kind !== "helper" && !win.kind.startsWith("x-") ? win.path : null;
    selectInTree(path);
    setPaletteTarget(path);
    $("empty").hidden = !!win;
    $("window-caption").hidden = !win;
    $("window-caption").textContent = win ? win.title : "";
  });
  api("info")
    .then((info) => {
      setLang(info.lang);
      setIconVersion(info.icons_v);
      applyLang();
      fill($("palette-body"), el("div", { class: "msg" }, T.pick));
      header(info);
      return initTree(showError).then(() => {
        if (parseHash()) openFromHash();
      });
    })
    .catch(showError);
}

start();
