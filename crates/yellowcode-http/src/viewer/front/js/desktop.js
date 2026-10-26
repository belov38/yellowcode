import { $, fill } from "./dom.js";
import { T } from "./i18n.js";


let busy = false;

let timer = null;

let dismissed = false;

let cancelBtn = null;

function banner(text, bad, cancel, ticking) {
  const b = $("banner");
  if (!text || (ticking && dismissed)) {
    b.hidden = true;
    return;
  }
  fill($("banner-text"), text);
  if (cancel) {
    if (!cancelBtn) {
      cancelBtn = document.createElement("button");
      cancelBtn.id = "banner-cancel";
      cancelBtn.type = "button";
      cancelBtn.textContent = T.cancel;
      $("banner-text").after(cancelBtn);
    }
    cancelBtn.onclick = cancel;
    cancelBtn.hidden = false;
  } else if (cancelBtn) {
    cancelBtn.hidden = true;
  }
  b.className = bad ? "bad" : "";
  b.hidden = false;
}

export let desktopItems = async () => [];
export let recentItems = null;
export let openSource = null;
export const desktopBusy = () => busy;

export function initDesktop() {
  const tauri = window.__TAURI__;
  if (!tauri || !tauri.core || !tauri.event) return;
  document.documentElement.classList.toggle("desktop-mac", /Mac/.test(navigator.platform));
  const invoke = tauri.core.invoke;
  $("banner-x").addEventListener("click", () => {
    dismissed = true;
    banner(null);
  });

  const fail = (e) => {
    dismissed = false;
    banner(String(e), true);
  };
  const openPath = (path) => {
    if (!path || busy) return;
    dismissed = false;
    banner(null);
    invoke("open_source", { path }).catch(fail);
  };
  const pick = (folder) => {
    if (busy) return;
    invoke("pick_source", { folder }).then(openPath).catch(fail);
  };

  openSource = () => pick(false);
  desktopItems = async () => [{ label: T.openFolder, disabled: busy, onclick: () => pick(true) }];
  recentItems = async () => {
    let recent = [];
    try {
      recent = await invoke("recent");
    } catch (e) {
      fail(e);
    }
    return recent.map((r, i) => ({
      label: i + 1 + " " + (r.label || r.path), title: r.path, accel: String((i + 1) % 10),
      disabled: busy, missing: !r.exists, onclick: () => openPath(r.path),
    }));
  };

  const stop = () => {
    busy = false;
    document.dispatchEvent(new Event("commands-changed"));
    if (timer !== null) clearInterval(timer);
    timer = null;
  };
  const loading = (path, elapsed) => {
    busy = true;
    document.dispatchEvent(new Event("commands-changed"));
    const t0 = Date.now() - (elapsed || 0);
    const show = () =>
      banner(T.loadingSource + path + " … " + Math.floor((Date.now() - t0) / 1000) + " s", false, () =>
        invoke("cancel_load").catch(fail), true);
    if (timer !== null) clearInterval(timer);
    dismissed = false;
    show();
    timer = setInterval(show, 1000);
  };
  const listen = tauri.event.listen;
  listen("source-loading", (e) => loading(e.payload.path, 0));
  listen("source-cancelled", () => {
    stop();
    banner(null);
  });
  listen("source-ready", () => {
    stop();
    history.replaceState(null, "", "/");
    location.reload();
  });
  listen("source-error", (e) => {
    stop();
    dismissed = false;
    banner(e.payload.path + ": " + e.payload.error, true);
  });
  invoke("status").then((s) => {
    if (s.state === "loading") loading(s.path, s.elapsed_ms);
  }, () => {});
}
