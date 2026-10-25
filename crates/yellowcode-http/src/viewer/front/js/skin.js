import { api } from "./api.js";
import { T } from "./i18n.js";
export let skinItems = async () => [];

export function initSkin(onError) {
  const invoke = window.__TAURI__?.core?.invoke;
  const reload = () => location.reload();
  const select = (id) => (invoke ? invoke("select_skin", { id })
    : fetch("/api/skins/select", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ id }) })
      .then(async (r) => { const data = await r.json(); if (!r.ok) throw new Error(data.error); return data; }))
    .then(reload).catch(onError);
  const install = (folder) => invoke("pick_skin", { folder }).then((path) => {
    if (path) return invoke("install_skin", { path }).then(reload);
  }).catch(onError);
  skinItems = async () => {
    const data = await api("skins");
    return [
      ...data.skins.map((skin) => ({ label: (skin.id === data.active ? "✓ " : "") + (skin.id === "builtin" ? (document.documentElement.lang === "en" ? "1C Classic" : "1С классический") : skin.name), onclick: () => select(skin.id) })),
      ...(invoke ? [
        { sep: true },
        { label: T.installSkinFile, onclick: () => install(false) },
        { label: T.installSkinFolder, onclick: () => install(true) },
      ] : []),
    ];
  };
}
