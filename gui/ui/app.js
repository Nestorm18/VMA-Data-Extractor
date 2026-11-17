"use strict";
const T = window.__TAURI__;
const invoke = T.core.invoke;
const $ = (id) => document.getElementById(id);

// ------------------------------------------------------------ utilidades
const nf1 = new Intl.NumberFormat("es-ES", { maximumFractionDigits: 1 });
function fmtBytes(n) {
  if (n == null) return "?";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${n} B` : `${nf1.format(v)} ${u[i]}`;
}
function fmtDate(secs, withTime = true) {
  if (secs == null) return "";
  const opts = { day: "numeric", month: "short", year: "numeric" };
  if (withTime) Object.assign(opts, { hour: "2-digit", minute: "2-digit" });
  return new Date(secs * 1000).toLocaleString("es-ES", opts);
}
function fmtEta(s) {
  if (s == null) return "";
  if (s < 60) return `quedan ${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `quedan ${m} min ${s % 60} s`;
  return `quedan ${Math.floor(m / 60)} h ${m % 60} min`;
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
let toastTimer;
function toast(msg) {
  const t = $("toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 3500);
}
const osIcon = (os) => (os === "windows" ? "windows" : os === "linux" ? "linux" : "other");

// ------------------------------------------------------------ diálogo
let modalResolve = null;
function modal({ title, body = "", actions = [], progress = false }) {
  $("modal-title").textContent = title;
  $("modal-body").innerHTML = body;
  $("modal-progress").hidden = !progress;
  const box = $("modal-actions");
  box.innerHTML = "";
  return new Promise((resolve) => {
    modalResolve = resolve;
    for (const a of actions) {
      const b = document.createElement("button");
      b.textContent = a.label;
      if (a.kind) b.className = a.kind;
      b.onclick = () => {
        if (a.keepOpen) { a.onClick && a.onClick(); return; }
        closeModal(a.value);
      };
      box.appendChild(b);
    }
    $("modal").hidden = false;
    hydrateIcons($("modal"));
  });
}
function closeModal(value) {
  $("modal").hidden = true;
  const r = modalResolve;
  modalResolve = null;
  if (r) r(value);
}
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("modal").hidden && $("modal-progress").hidden) closeModal(null);
});

// ------------------------------------------------------------ trabajos
const jobs = new Map(); // id -> { onProgress, onDone }
T.event.listen("job-progress", (e) => jobs.get(e.payload.id)?.onProgress?.(e.payload));
T.event.listen("job-done", (e) => {
  const j = jobs.get(e.payload.id);
  jobs.delete(e.payload.id);
  j?.onDone?.(e.payload);
});
async function runJob(cmd, args, handlers) {
  const id = await invoke(cmd, args);
  jobs.set(id, handlers);
  return id;
}

// ------------------------------------------------------------ vistas
const state = {
  view: "home",
  backup: null,
  dataBytes: null,
  dataSource: null,
  checksum: null,
  analyzeJob: null,
  extractJob: null,
  explorer: null,
};

function show(view) {
  state.view = view;
  for (const v of ["home", "backup", "explorer"]) $(`view-${v}`).hidden = v !== view;
  $("back").hidden = view === "home";
  if (view === "home") {
    $("title").textContent = "VMA Data Extractor";
    $("top-extra").textContent = "";
    loadRecents();
  }
}

$("back").onclick = async () => {
  if (state.view === "backup") {
    if (state.extractJob) {
      const ok = await modal({ title: "Hay una extracción en curso", body: "<p>¿Quieres cancelarla y volver al inicio?</p>", actions: [{ label: "Seguir extrayendo", value: false }, { label: "Cancelar extracción", value: true, kind: "danger" }] });
      if (!ok) return;
      await invoke("cancel_job", { id: state.extractJob });
    }
    if (state.analyzeJob) await invoke("cancel_job", { id: state.analyzeJob });
    show("home");
  } else if (state.view === "explorer") {
    if (await askCloseExtraction()) show(state.backup ? "backup" : "home");
  }
};

// ------------------------------------------------------------ inicio
async function loadRecents() {
  const list = await invoke("get_recents");
  $("recents-wrap").hidden = list.length === 0;
  $("recents").innerHTML = list
    .map(
      (r, i) => `<div class="list-item" data-i="${i}">${icon(osIcon(r.os))}
        <span class="grow ellipsis">${esc(r.name)}${r.vmid ? ` · VM ${esc(r.vmid)}` : ""}</span>
        <span class="muted small">${fmtDate(r.created, false)} · ${fmtBytes(r.file_size)}</span></div>`
    )
    .join("");
  $("recents").querySelectorAll(".list-item").forEach((el) => (el.onclick = () => openPath(list[+el.dataset.i].path)));
}

$("open-backup").onclick = async () => {
  const p = await invoke("pick_backup");
  if (p) openPath(p);
};
$("open-folder").onclick = async () => {
  const p = await invoke("pick_folder", { title: "Abrir carpeta ya extraída", start: null });
  if (p) openExtraction(p);
};

async function openPath(p) {
  if (await invoke("is_dir", { path: p })) return openExtraction(p);
  return loadBackup(p);
}

// arrastrar y soltar
T.webview.getCurrentWebview().onDragDropEvent((e) => {
  const t = e.payload.type;
  if (t === "enter" || t === "over") $("dropping").hidden = state.view === "explorer" || !!state.extractJob;
  else $("dropping").hidden = true;
  if (t === "drop" && e.payload.paths?.length && state.view !== "explorer" && !state.extractJob) openPath(e.payload.paths[0]);
});

// ------------------------------------------------------------ backup
async function loadBackup(path) {
  let info;
  try {
    info = await invoke("backup_info", { path });
  } catch (e) {
    await modal({ title: "No se puede abrir el backup", body: `<p>${esc(e)}</p>`, actions: [{ label: "Aceptar", kind: "primary" }] });
    return;
  }
  if (state.analyzeJob) invoke("cancel_job", { id: state.analyzeJob });
  Object.assign(state, { backup: info, dataBytes: info.log_data_bytes, dataSource: info.log_data_bytes != null ? "log" : null, checksum: null, analyzeJob: null });

  $("title").textContent = info.file_name;
  $("vm-icon").innerHTML = icon(osIcon(info.os));
  $("vm-name").textContent = info.name;
  const meta = [info.vmid ? `VM ${info.vmid}` : null, info.cores ? `${info.cores} núcleos` : null, info.memory_mb ? `${fmtBytes(info.memory_mb * 1048576)} RAM` : null, `copia del ${fmtDate(info.created)}`, `${fmtBytes(info.file_size)} comprimido`];
  $("vm-meta").textContent = meta.filter(Boolean).join(" · ");
  renderDisks();
  $("outdir").value = info.suggested_outdir;
  $("ignore-checksum").checked = false;
  $("checksum-warn").hidden = true;
  $("extract-box").hidden = true;
  $("extract-result").hidden = true;
  setChecksumPill();
  show("backup");
  updateSpace();
  if (state.dataBytes == null) startAnalyze(false);
}

function renderDisks(report) {
  const info = state.backup;
  $("disks").innerHTML = info.disks
    .map((d) => {
      const r = report?.disks.find((x) => x.name.replace(/^drive-/, "") === d.name);
      const sub = r ? (r.data_bytes === 0 ? "vacío" : `${fmtBytes(r.data_bytes)} con datos`) : "";
      return `<div class="disk"><p class="label">${esc(d.name)}</p><b>${fmtBytes(d.size)}</b><span class="muted small">${sub}</span></div>`;
    })
    .join("");
}

function setChecksumPill() {
  const p = $("checksum-pill");
  p.hidden = false;
  if (state.checksum === "ok") { p.className = "pill ok"; p.textContent = "Checksum OK"; }
  else if (state.checksum === "bad") { p.className = "pill bad"; p.textContent = "Backup dañado"; }
  else if (state.analyzeJob) { p.className = "pill info"; p.textContent = "Comprobando…"; }
  else { p.className = "pill info"; p.textContent = "Sin verificar"; }
}

async function startAnalyze(manual) {
  if (state.analyzeJob || !state.backup) return;
  const path = state.backup.path;
  $("analyze-box").hidden = false;
  $("analyze-text").textContent = manual ? "Verificando el backup…" : "Analizando el backup para calcular el espacio necesario…";
  $("analyze-bar").style.width = "0%";
  $("analyze-stats").textContent = "";
  state.analyzeJob = await runJob("start_analyze", { path }, {
    onProgress: (p) => {
      $("analyze-bar").style.width = `${(p.fraction * 100).toFixed(1)}%`;
      $("analyze-stats").textContent = `${Math.round(p.fraction * 100)} % · ${fmtBytes(p.bytes_per_sec)}/s ${p.eta_secs != null ? "· " + fmtEta(p.eta_secs) : ""}`;
    },
    onDone: (d) => {
      state.analyzeJob = null;
      $("analyze-box").hidden = true;
      if (state.backup?.path !== path) return;
      if (d.cancelled) { setChecksumPill(); updateSpace(); return; }
      const r = d.result;
      if (r) {
        state.dataBytes = r.disks.reduce((a, x) => a + x.data_bytes, 0);
        state.dataSource = "scan";
        renderDisks(r);
        state.checksum = r.complete && r.errors.length === 0 ? "ok" : "bad";
        if (state.checksum === "bad") {
          $("ignore-checksum").checked = true;
          $("checksum-warn").hidden = false;
        }
        if (manual && state.checksum === "ok") toast("El backup está correcto");
      } else {
        state.checksum = "bad";
        $("checksum-warn").hidden = false;
        $("ignore-checksum").checked = true;
      }
      setChecksumPill();
      updateSpace();
    },
  });
  setChecksumPill();
}
$("analyze-cancel").onclick = () => state.analyzeJob && invoke("cancel_job", { id: state.analyzeJob });
$("verify").onclick = () => {
  if (state.analyzeJob) return toast("Ya se está comprobando el backup");
  startAnalyze(true);
};

function neededBytes(space) {
  if (!space.sparse) return state.backup.total_disk_size;
  if (state.dataBytes == null) return null;
  return Math.ceil(state.dataBytes * 1.03) + 64 * 1048576;
}

let spaceSeq = 0;
async function updateSpace() {
  if (!state.backup) return null;
  const seq = ++spaceSeq;
  const space = await invoke("space_info", { outdir: $("outdir").value });
  if (seq !== spaceSeq) return null;
  const el = $("space");
  const need = neededBytes(space);
  el.className = "small space";
  if (need == null) {
    el.innerHTML = `${icon("clock")} Calculando el espacio necesario… · libres ${fmtBytes(space.free)}`;
  } else if (space.free != null && space.free < need) {
    el.className = "small space bad";
    el.innerHTML = `${icon("alert")} No hay espacio suficiente: necesita unos ${fmtBytes(need)} y hay ${fmtBytes(space.free)} libres`;
  } else {
    el.className = space.sparse ? "small space ok" : "small space warn";
    const src = state.dataSource === "log" ? " (según el log de vzdump)" : "";
    el.innerHTML = space.sparse
      ? `${icon("check")} Necesita unos ${fmtBytes(need)}${src} · libres ${fmtBytes(space.free)}`
      : `${icon("alert")} Este destino no admite archivos dispersos: ocupará ${fmtBytes(need)} · libres ${fmtBytes(space.free)}`;
  }
  return { space, need };
}
let outdirTimer;
$("outdir").addEventListener("input", () => {
  clearTimeout(outdirTimer);
  outdirTimer = setTimeout(updateSpace, 300);
});
$("pick-outdir").onclick = async () => {
  const p = await invoke("pick_folder", { title: "Elegir dónde extraer", start: $("outdir").value });
  if (p) {
    $("outdir").value = p;
    updateSpace();
  }
};

$("extract").onclick = () => doExtract(false);

async function doExtract(overwrite, forceIgnore) {
  if (state.extractJob) return;
  const outdir = $("outdir").value.trim();
  if (!outdir) return toast("Indica una carpeta de destino");
  const st = await updateSpace();
  if (!st) return;
  if (st.need == null && state.analyzeJob) {
    const r = await modal({
      title: "Calculando el espacio necesario",
      body: "<p>Todavía se está analizando el backup para saber cuánto ocupará. Espera a que termine para comprobar que hay espacio, o extrae ya sin comprobarlo.</p>",
      actions: [{ label: "Esperar", value: false }, { label: "Extraer sin comprobar", value: true }],
    });
    if (!r) return;
    invoke("cancel_job", { id: state.analyzeJob });
  } else if (st.need == null) {
    const r = await modal({
      title: "Espacio necesario desconocido",
      body: `<p>No se ha podido calcular cuánto ocupará. En el peor caso, hasta ${fmtBytes(state.backup.total_disk_size)} (el tamaño total de los discos). Libres: ${fmtBytes(st.space.free)}.</p>`,
      actions: [{ label: "Cancelar", value: false }, { label: "Extraer igualmente", value: true, kind: "primary" }],
    });
    if (!r) return;
  }
  if (st.need != null && st.space.free != null && st.space.free < st.need) {
    const r = await modal({
      title: "No hay espacio suficiente",
      body: `<p>La extracción necesita unos <b>${fmtBytes(st.need)}</b> y en el destino solo hay <b>${fmtBytes(st.space.free)}</b> libres.</p><p>Elige otra carpeta (por ejemplo en otro disco) o libera espacio. Si extraes igualmente, se parará al llenarse el disco.</p>`,
      actions: [{ label: "Cancelar", value: "cancel" }, { label: "Extraer igualmente", value: "force" }, { label: "Elegir otra carpeta", value: "pick", kind: "primary" }],
    });
    if (r === "pick") return $("pick-outdir").onclick();
    if (r !== "force") return;
  }
  if (st.space.existing && !overwrite) {
    const r = await modal({
      title: "Ya hay una extracción en esta carpeta",
      body: `<p>${esc(outdir)} ya contiene discos extraídos.</p>`,
      actions: [{ label: "Cancelar", value: null }, { label: "Sobrescribir", value: "over", kind: "danger" }, { label: "Abrir la existente", value: "open", kind: "primary" }],
    });
    if (r === "open") return openExtraction(outdir);
    if (r !== "over") return;
    overwrite = true;
  }
  if (forceIgnore) $("ignore-checksum").checked = true;
  startExtract(outdir, overwrite);
}

async function startExtract(outdir, overwrite) {
  const path = state.backup.path;
  const ignore = $("ignore-checksum").checked;
  $("extract-box").hidden = false;
  $("extract-result").hidden = true;
  $("extract-bar").style.width = "0%";
  $("extract-text").textContent = "Extrayendo…";
  $("extract-stats").textContent = "0 %";
  $("extract-detail").textContent = "";
  const started = Date.now();
  state.extractJob = await runJob("start_extract", { path, outdir, ignoreChecksum: ignore, overwrite }, {
    onProgress: (p) => {
      $("extract-bar").style.width = `${(p.fraction * 100).toFixed(1)}%`;
      $("extract-stats").textContent = `${Math.round(p.fraction * 100)} % · ${p.eta_secs != null ? fmtEta(p.eta_secs) : "calculando tiempo…"}`;
      $("extract-detail").textContent = `${fmtBytes(p.done_bytes)} de ${fmtBytes(p.total_bytes)} leídos · ${fmtBytes(p.bytes_per_sec)}/s`;
    },
    onDone: (d) => {
      state.extractJob = null;
      $("extract-box").hidden = true;
      showExtractResult(d, outdir, Math.round((Date.now() - started) / 1000));
    },
  });
}
$("extract-cancel").onclick = () => state.extractJob && invoke("cancel_job", { id: state.extractJob });

function showExtractResult(d, outdir, secs) {
  const el = $("extract-result");
  el.hidden = false;
  const r = d.result;
  const errs = r?.errors?.filter((e) => !e.includes("cancelado")) ?? [];
  if (d.ok && r?.complete && errs.length === 0) {
    el.innerHTML = `<div class="banner ok">${icon("check")}<div>Extracción terminada en ${secs} s. Abriendo el explorador…</div></div>`;
    setTimeout(() => openExtraction(outdir), 700);
    return;
  }
  let html;
  if (d.cancelled) {
    html = `<div class="banner warn">${icon("alert")}<div>Extracción cancelada. Lo extraído hasta ahora sigue en la carpeta de destino.</div></div>`;
  } else {
    const msg = d.error ? esc(d.error) : r?.checksum_error ? "El checksum del backup no coincide: el archivo está dañado." : "La extracción no se completó.";
    html = `<div class="banner bad">${icon("alert")}<div><b>${msg}</b>${errs.length ? `<div class="small" style="margin-top:4px">${errs.slice(0, 4).map(esc).join("<br>")}</div>` : ""}</div></div>`;
  }
  const buttons = [];
  if (r?.checksum_error && !$("ignore-checksum").checked) buttons.push(`<button id="retry-ignore" class="primary">Reintentar ignorando checksum</button>`);
  if (r) buttons.push(`<button id="open-partial">Abrir lo extraído</button>`);
  el.innerHTML = html + `<div class="row gap" style="margin-top:10px">${buttons.join("")}</div>`;
  if ($("retry-ignore")) $("retry-ignore").onclick = () => doExtract(true, true);
  if ($("open-partial")) $("open-partial").onclick = () => openExtraction(outdir);
}

// ------------------------------------------------------------ explorador
async function openExtraction(dir) {
  let info;
  try {
    info = await invoke("open_extraction", { dir });
  } catch (e) {
    await modal({ title: "No se puede abrir la extracción", body: `<p>${esc(e)}</p>`, actions: [{ label: "Aceptar", kind: "primary" }] });
    return;
  }
  state.explorer = { info, vol: null, path: [], entries: [], selected: new Set() };
  $("title").textContent = info.name;
  $("top-extra").textContent = info.dir;
  $("usage").textContent = `Ocupa ${fmtBytes(info.disk_usage)} en disco`;
  renderVols();
  show("explorer");
  const first = info.vols.find((v) => v.browsable);
  if (first) selectVol(first.id);
  else {
    $("rows").innerHTML = "";
    showEmpty("Ningún volumen de este backup se puede explorar (sistemas de archivos no soportados).");
  }
}

function renderVols() {
  const ex = state.explorer;
  $("vols").innerHTML = ex.info.vols
    .map(
      (v) => `<div class="vol ${v.id === ex.vol ? "active" : ""} ${v.browsable ? "" : "disabled"}" data-id="${esc(v.id)}" title="${esc(v.detail)}">
        ${icon(v.icon)}<div class="grow ellipsis">${esc(v.label)}<small>${fmtBytes(v.size)}${v.browsable ? "" : " · no navegable"}</small></div></div>`
    )
    .join("");
  $("vols").querySelectorAll(".vol").forEach((el) => {
    el.onclick = () => {
      const v = ex.info.vols.find((x) => x.id === el.dataset.id);
      if (!v.browsable) return toast(`${v.label}: ${v.detail}`);
      selectVol(v.id);
    };
  });
}

function selectVol(id) {
  state.explorer.vol = id;
  renderVols();
  navigate([]);
}

async function navigate(path) {
  const ex = state.explorer;
  $("filter").value = "";
  $("rows").innerHTML = "";
  showEmpty("Cargando…");
  try {
    const entries = await invoke("list_dir", { vol: ex.vol, path: "/" + path.join("/") });
    ex.path = path;
    ex.entries = entries;
    ex.selected = new Set();
    renderCrumbs();
    renderRows();
  } catch (e) {
    showEmpty(`No se puede abrir esta carpeta: ${e}`);
  }
}

function renderCrumbs() {
  const ex = state.explorer;
  const vol = ex.info.vols.find((v) => v.id === ex.vol);
  const parts = [{ label: vol.label, path: [] }, ...ex.path.map((p, i) => ({ label: p, path: ex.path.slice(0, i + 1) }))];
  $("crumbs").innerHTML = parts.map((p, i) => `${i ? `<span class="sep">${icon("chevron")}</span>` : ""}<a data-i="${i}">${esc(p.label)}</a>`).join("");
  $("crumbs").querySelectorAll("a").forEach((a) => (a.onclick = () => navigate(parts[+a.dataset.i].path)));
}

function showEmpty(msg) {
  $("files-empty").hidden = !msg;
  $("files-empty").textContent = msg || "";
}

function visibleEntries() {
  const f = $("filter").value.trim().toLowerCase();
  return state.explorer.entries.filter((e) => !f || e.name.toLowerCase().includes(f));
}

function renderRows() {
  const ex = state.explorer;
  const list = visibleEntries();
  showEmpty(list.length ? "" : ex.entries.length ? "Nada coincide con la búsqueda." : "Carpeta vacía.");
  $("rows").innerHTML = list
    .map((e) => {
      const ic = e.is_link ? icon("link", "link") : e.is_dir ? icon("folder", "dir") : icon("file");
      const sel = ex.selected.has(e.name);
      return `<tr class="item ${sel ? "selected" : ""}" data-name="${esc(e.name)}">
        <td class="c-check"><input type="checkbox" ${sel ? "checked" : ""} ${e.is_link ? "disabled" : ""}></td>
        <td><div class="name-cell ${e.is_link ? "dim" : ""}">${ic}<span class="ellipsis">${esc(e.name)}</span></div></td>
        <td class="c-size">${e.is_dir || e.is_link ? "" : fmtBytes(e.size)}</td>
        <td class="c-date">${fmtDate(e.mtime)}</td></tr>`;
    })
    .join("");
  $("rows").querySelectorAll("tr").forEach((tr) => {
    const e = ex.entries.find((x) => x.name === tr.dataset.name);
    const cb = tr.querySelector("input");
    cb.onclick = (ev) => {
      ev.stopPropagation();
      toggle(e.name, cb.checked);
    };
    tr.onclick = (ev) => {
      if (e.is_link) return;
      if (ev.ctrlKey || ev.metaKey) toggle(e.name, !ex.selected.has(e.name));
      else {
        ex.selected = new Set([e.name]);
        syncSelection();
      }
    };
    tr.ondblclick = () => {
      if (e.is_dir && !e.is_link) navigate([...ex.path, e.name]);
    };
  });
  updateSelection();
}

function toggle(name, on) {
  const s = state.explorer.selected;
  on ? s.add(name) : s.delete(name);
  syncSelection();
}

// Actualiza casillas y resaltado sin reconstruir la tabla.
function syncSelection() {
  const s = state.explorer.selected;
  $("rows").querySelectorAll("tr").forEach((tr) => {
    const on = s.has(tr.dataset.name);
    tr.classList.toggle("selected", on);
    tr.querySelector("input").checked = on;
  });
  updateSelection();
}

function updateSelection() {
  const ex = state.explorer;
  const n = ex.selected.size;
  const vis = visibleEntries().filter((e) => !e.is_link);
  $("select-all").checked = vis.length > 0 && vis.every((e) => ex.selected.has(e.name));
  if (n === 0) {
    $("sel-count").textContent = `${ex.entries.length} elementos · doble clic para abrir una carpeta`;
  } else {
    const bytes = ex.entries.filter((e) => ex.selected.has(e.name) && !e.is_dir).reduce((a, e) => a + e.size, 0);
    const dirs = ex.entries.filter((e) => ex.selected.has(e.name) && e.is_dir).length;
    $("sel-count").textContent = `${n} seleccionado${n > 1 ? "s" : ""}${bytes ? ` · ${fmtBytes(bytes)}` : ""}${dirs ? ` · ${dirs} carpeta${dirs > 1 ? "s" : ""}` : ""}`;
  }
}

$("select-all").onclick = () => {
  const ex = state.explorer;
  const vis = visibleEntries().filter((e) => !e.is_link);
  if ($("select-all").checked) vis.forEach((e) => ex.selected.add(e.name));
  else vis.forEach((e) => ex.selected.delete(e.name));
  syncSelection();
};
$("filter").addEventListener("input", renderRows);

$("copy").onclick = async () => {
  const ex = state.explorer;
  if (!ex?.vol) return;
  let names = [...ex.selected];
  if (names.length === 0) {
    const vol = ex.info.vols.find((v) => v.id === ex.vol);
    const here = ex.path.length ? ex.path[ex.path.length - 1] : vol.label;
    const r = await modal({
      title: "No hay nada seleccionado",
      body: `<p>Marca las casillas de los archivos o carpetas que quieras copiar. ¿O prefieres copiar la carpeta actual entera (<b>${esc(here)}</b>)?</p>`,
      actions: [{ label: "Cancelar", value: false }, { label: "Copiar carpeta actual", value: true, kind: "primary" }],
    });
    if (!r) return;
    names = null;
  }
  const dest = await invoke("pick_folder", { title: "Copiar a…", start: null });
  if (!dest) return;
  const base = "/" + ex.path.join("/");
  const paths = names ? names.map((n) => (base === "/" ? "/" : base + "/") + n) : [base];
  startCopy(paths, dest);
};

async function startCopy(paths, dest) {
  const ex = state.explorer;
  let jobId = null;
  const done = new Promise((resolve) => {
    runJob("start_copy", { vol: ex.vol, paths, dest }, {
      onProgress: (p) => {
        const bar = $("modal-bar").parentElement;
        bar.classList.toggle("indeterminate", p.total_bytes === 0 && p.fraction === 0);
        $("modal-bar").style.width = `${(p.fraction * 100).toFixed(1)}%`;
        $("modal-ptext").textContent = p.text;
        $("modal-pstats").textContent = p.total_bytes ? `${Math.round(p.fraction * 100)} % · ${fmtBytes(p.done_bytes)} de ${fmtBytes(p.total_bytes)}${p.eta_secs != null ? " · " + fmtEta(p.eta_secs) : ""}` : "";
      },
      onDone: resolve,
    }).then((id) => (jobId = id), (e) => resolve({ ok: false, error: String(e) }));
  });
  $("modal-bar").style.width = "0%";
  $("modal-ptext").textContent = "Preparando…";
  $("modal-pstats").textContent = "";
  modal({
    title: "Copiando",
    body: `<p class="ellipsis">a ${esc(dest)}</p>`,
    progress: true,
    actions: [{ label: "Cancelar", keepOpen: true, onClick: () => jobId && invoke("cancel_job", { id: jobId }) }],
  });
  const d = await done;
  $("modal-progress").hidden = true;
  const st = d.result?.stats;
  let body;
  if (d.error) body = `<p>${esc(d.error)}</p>`;
  else {
    body = `<p>${d.cancelled ? "Copia cancelada. " : ""}Copiados <b>${st.files}</b> archivos (${fmtBytes(st.bytes)}) a ${esc(dest)}.</p>`;
    if (st.skipped) body += `<p>${st.skipped} enlaces o puntos de unión omitidos.</p>`;
    if (st.errors.length) body += `<p>${st.errors.length} archivo${st.errors.length > 1 ? "s" : ""} no se pudieron copiar:</p><div class="errors">${st.errors.slice(0, 100).map(esc).join("\n")}</div>`;
  }
  const r = await modal({
    title: d.error ? "No se pudo copiar" : d.cancelled ? "Copia cancelada" : st.errors.length ? "Copia terminada con errores" : "Copia terminada",
    body,
    actions: [{ label: "Cerrar", value: false }, ...(d.error ? [] : [{ label: "Abrir carpeta", value: true, kind: "primary" }])],
  });
  if (r) invoke("reveal", { path: dest });
}

// ------------------------------------------------------------ cerrar extracción
async function askCloseExtraction(quitting = false) {
  const ex = state.explorer;
  const r = await modal({
    title: "¿Borrar los archivos extraídos?",
    body: `<p>Ocupan <b>${fmtBytes(ex.info.disk_usage)}</b> en ${esc(ex.info.dir)}.</p><p>Si los conservas podrás volver a abrirlos sin extraer otra vez («Abrir carpeta ya extraída»).</p>`,
    actions: [{ label: "Cancelar", value: null }, { label: "Conservar", value: "keep" }, { label: "Borrar", value: "delete", kind: "danger" }],
  });
  if (!r) return false;
  if (quitting) {
    await invoke("quit_app", { delete: r === "delete" });
    return true;
  }
  const freed = await invoke("close_extraction", { delete: r === "delete" });
  if (r === "delete") toast(`Borrado: ${fmtBytes(freed)} liberados`);
  state.explorer = null;
  return true;
}
$("close-extraction").onclick = async () => {
  if (await askCloseExtraction()) show(state.backup ? "backup" : "home");
};
T.event.listen("close-requested", () => askCloseExtraction(true));

document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest("input")) e.preventDefault();
});

hydrateIcons();
show("home");
T.app?.getVersion?.().then((v) => ($("version").textContent = `v${v}`)).catch(() => {});
