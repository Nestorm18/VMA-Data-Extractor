// Interfaz gráfica de vmatool (Tauri 2). La interfaz está en ui/ (HTML/JS sin
// dependencias) y se comunica con estos comandos mediante invoke/eventos.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use vmatool::volumes::{FsKind, Volume};
use vmatool::{fs, sparse, vma, volumes};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Default)]
struct AppState {
    next_job: AtomicU64,
    jobs: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    explorer: Mutex<Option<Explorer>>,
    close_ok: AtomicBool,
}

struct Explorer {
    dir: PathBuf,
    vols: Vec<Volume>,
}

// ------------------------------------------------------------------ backup

#[derive(Serialize, Clone)]
struct DiskInfo {
    name: String,
    size: u64,
}

#[derive(Serialize, Clone)]
struct BackupInfo {
    path: String,
    file_name: String,
    file_size: u64,
    vmid: Option<String>,
    name: String,
    os: String,
    cores: Option<u64>,
    memory_mb: Option<u64>,
    created: i64,
    disks: Vec<DiskInfo>,
    total_disk_size: u64,
    /// Datos reales según el .log de vzdump (si existe)
    log_data_bytes: Option<u64>,
    suggested_outdir: String,
    config: String,
}

fn conf_value<'a>(conf: &'a str, key: &str) -> Option<&'a str> {
    conf.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')).map(|v| v.trim()))
}

fn os_family(ostype: &str) -> &'static str {
    if ostype.starts_with("win") || ostype == "wxp" || ostype == "w2k" || ostype.starts_with("w2k") {
        "windows"
    } else if ostype.starts_with('l') {
        "linux"
    } else {
        "other"
    }
}

fn vmid_from_name(file: &str) -> Option<String> {
    let rest = file.strip_prefix("vzdump-qemu-")?;
    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!id.is_empty()).then_some(id)
}

#[tauri::command(async)]
fn backup_info(app: AppHandle, path: String) -> CmdResult<BackupInfo> {
    let p = PathBuf::from(&path);
    let mut s = vma::open(&p, false).map_err(err)?;
    let h = vma::read_header(&mut s).map_err(|e| format!("{e:#}"))?;
    let conf = h
        .configs
        .iter()
        .find(|(n, _)| n == "qemu-server.conf")
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .unwrap_or_default();
    let file_name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let vmid = vmid_from_name(&file_name);
    let name = conf_value(&conf, "name").unwrap_or("VM").to_string();
    let disks: Vec<DiskInfo> = h.devices.iter().map(|d| DiskInfo { name: d.name.trim_start_matches("drive-").to_string(), size: d.size }).collect();
    let folder = match &vmid {
        Some(id) => format!("{name}-{id}"),
        None => name.clone(),
    };
    let folder: String = folder.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let info = BackupInfo {
        file_size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
        os: os_family(conf_value(&conf, "ostype").unwrap_or("")).to_string(),
        cores: conf_value(&conf, "cores").and_then(|v| v.parse().ok()),
        memory_mb: conf_value(&conf, "memory").and_then(|v| v.parse().ok()),
        created: h.ctime,
        total_disk_size: disks.iter().map(|d| d.size).sum(),
        disks,
        log_data_bytes: vma::data_size_from_log(&p).map(|(d, _)| d),
        suggested_outdir: p.parent().unwrap_or(Path::new(".")).join(folder).to_string_lossy().into_owned(),
        config: conf,
        path,
        file_name,
        vmid,
        name,
    };
    add_recent(&app, &info);
    Ok(info)
}

#[derive(Serialize)]
struct SpaceInfo {
    free: Option<u64>,
    sparse: bool,
    /// Ya hay una extracción en esa carpeta
    existing: bool,
}

#[tauri::command(async)]
fn space_info(outdir: String) -> SpaceInfo {
    let p = PathBuf::from(&outdir);
    let (free, sparse) = match sparse::volume_info(&p) {
        Some((f, s)) => (Some(f), s),
        None => (None, true),
    };
    SpaceInfo { free, sparse, existing: has_extraction(&p) }
}

fn has_extraction(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|it| {
            it.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("disk-") && n.ends_with(".raw")
            })
        })
        .unwrap_or(false)
}

// ------------------------------------------------------------------ trabajos

#[derive(Serialize, Clone)]
struct JobProgress {
    id: u64,
    kind: String,
    fraction: f64,
    eta_secs: Option<u64>,
    bytes_per_sec: u64,
    done_bytes: u64,
    total_bytes: u64,
    text: String,
}

#[derive(Serialize, Clone)]
struct JobDone<T: Serialize + Clone> {
    id: u64,
    kind: String,
    ok: bool,
    cancelled: bool,
    error: Option<String>,
    result: Option<T>,
}

fn new_job(state: &AppState) -> (u64, Arc<AtomicBool>) {
    let id = state.next_job.fetch_add(1, Ordering::Relaxed) + 1;
    let flag = Arc::new(AtomicBool::new(false));
    state.jobs.lock().unwrap().insert(id, flag.clone());
    (id, flag)
}

fn finish_job<T: Serialize + Clone>(app: &AppHandle, id: u64, kind: &str, res: anyhow::Result<T>, cancelled: bool) {
    app.state::<AppState>().jobs.lock().unwrap().remove(&id);
    let done = match res {
        Ok(r) => JobDone { id, kind: kind.into(), ok: !cancelled, cancelled, error: None, result: Some(r) },
        Err(e) => JobDone { id, kind: kind.into(), ok: false, cancelled, error: Some(format!("{e:#}")), result: None },
    };
    let _ = app.emit("job-done", done);
}

fn vma_progress(app: &AppHandle, id: u64, kind: &str, p: &vma::Progress) {
    let _ = app.emit(
        "job-progress",
        JobProgress {
            id,
            kind: kind.into(),
            fraction: p.fraction,
            eta_secs: p.eta_secs,
            bytes_per_sec: p.bytes_per_sec,
            done_bytes: p.input_read,
            total_bytes: p.input_size,
            text: String::new(),
        },
    );
}

#[tauri::command]
fn cancel_job(state: State<AppState>, id: u64) {
    if let Some(f) = state.jobs.lock().unwrap().get(&id) {
        f.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn start_analyze(app: AppHandle, state: State<AppState>, path: String) -> u64 {
    let (id, cancel) = new_job(&state);
    std::thread::spawn(move || {
        let res = vma::scan_with(Path::new(&path), |p| vma_progress(&app, id, "analyze", p), &cancel);
        let cancelled = cancel.load(Ordering::Relaxed);
        finish_job(&app, id, "analyze", res, cancelled);
    });
    id
}

#[tauri::command]
fn start_extract(app: AppHandle, state: State<AppState>, path: String, outdir: String, ignore_checksum: bool, overwrite: bool) -> u64 {
    let (id, cancel) = new_job(&state);
    std::thread::spawn(move || {
        let res = vma::extract_with(
            Path::new(&path),
            Path::new(&outdir),
            ignore_checksum,
            overwrite,
            |p| vma_progress(&app, id, "extract", p),
            &cancel,
        );
        let cancelled = cancel.load(Ordering::Relaxed);
        finish_job(&app, id, "extract", res, cancelled);
    });
    id
}

// ------------------------------------------------------------------ explorador

#[derive(Serialize, Clone)]
struct VolDto {
    id: String,
    label: String,
    detail: String,
    kind: String,
    icon: String,
    size: u64,
    browsable: bool,
}

#[derive(Serialize)]
struct ExtractionInfo {
    dir: String,
    name: String,
    os: String,
    vols: Vec<VolDto>,
    disk_usage: u64,
}

fn friendly(disks: &[volumes::Disk], v: &Volume, fstab: &HashMap<String, String>) -> VolDto {
    let mut dto = VolDto {
        id: v.id.clone(),
        label: v.id.clone(),
        detail: format!("{} · {}", v.kind.label(), v.id),
        kind: v.kind.label().into(),
        icon: "disk".into(),
        size: v.size,
        browsable: v.kind.browsable(),
    };
    let desc = v.desc.to_lowercase();
    match v.kind {
        FsKind::Ntfs => {
            dto.icon = "windows".into();
            let is_system = fs::open(disks, v).ok().and_then(|mut f| f.list(&[]).ok()).is_some_and(|l| l.iter().any(|e| e.is_dir && e.name.eq_ignore_ascii_case("Windows")));
            dto.label = if is_system {
                "Windows (C:)".into()
            } else if desc.contains("recuperación") {
                dto.icon = "lifebuoy".into();
                "Recuperación".into()
            } else {
                "Datos NTFS".into()
            };
        }
        FsKind::Ext => {
            dto.icon = "linux".into();
            let mount = fstab.get(&v.id.to_lowercase()).cloned();
            dto.label = match mount.as_deref() {
                Some("/") => "Linux (/)".into(),
                Some(m) => m.to_string(),
                None => "Linux".into(),
            };
            if mount.is_none() && ext_root_has(disks, v, "etc") {
                dto.label = "Linux (/)".into();
            }
        }
        FsKind::Fat => {
            dto.icon = "bolt".into();
            dto.label = if desc.contains("efi") { "EFI".into() } else { "FAT".into() };
            dto.browsable = false;
        }
        FsKind::Swap => {
            dto.icon = "swap".into();
            dto.label = "Swap".into();
        }
        FsKind::LvmPv => {
            dto.icon = "stack".into();
            dto.label = "LVM".into();
            dto.detail = format!("contiene volúmenes lógicos · {}", v.id);
        }
        _ => {
            if desc.contains("reservada") {
                dto.label = "Reservada de Microsoft".into();
            } else if v.id.contains("efidisk") {
                dto.label = "Variables EFI".into();
                dto.detail = format!("disco de la VM · {}", v.id);
            } else if v.id.contains("tpmstate") {
                dto.label = "TPM".into();
                dto.detail = format!("disco de la VM · {}", v.id);
            } else if is_empty_volume(disks, v) {
                dto.label = format!("{} · vacío", v.id);
                dto.detail = "sin datos".into();
            } else {
                dto.detail = format!("{} · {}", v.desc, v.id);
            }
        }
    }
    dto
}

fn ext_root_has(disks: &[volumes::Disk], v: &Volume, name: &str) -> bool {
    fs::open(disks, v).ok().and_then(|mut f| f.list(&[]).ok()).is_some_and(|l| l.iter().any(|e| e.name == name))
}

fn is_empty_volume(disks: &[volumes::Disk], v: &Volume) -> bool {
    // disco entero cuyo .raw no ocupa casi nada
    v.segs.len() == 1
        && v.segs[0].disk_off == 0
        && v.size == disks[v.segs[0].disk].size
        && sparse::allocated_size(&disks[v.segs[0].disk].path).is_some_and(|a| a < 1024 * 1024)
}

/// Lee /etc/fstab del sistema Linux para dar nombre (/home, /var...) a los volúmenes.
fn read_fstab(disks: &[volumes::Disk], vols: &[Volume]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut uuids: HashMap<String, String> = HashMap::new(); // uuid -> vol id
    let mut text = None;
    for v in vols.iter().filter(|v| v.kind == FsKind::Ext) {
        let Ok(fs::Fs::Ext(e)) = fs::open(disks, v) else { continue };
        uuids.insert(e.uuid().to_string().to_lowercase(), v.id.to_lowercase());
        if text.is_none()
            && let Ok(t) = e.read_to_string("/etc/fstab")
        {
            text = Some(t);
        }
    }
    let Some(text) = text else { return out };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(mnt)) = (it.next(), it.next()) else { continue };
        if dev.starts_with('#') {
            continue;
        }
        let id = if let Some(u) = dev.strip_prefix("UUID=") {
            uuids.get(&u.trim_matches('"').to_lowercase()).cloned()
        } else if let Some(m) = dev.strip_prefix("/dev/mapper/") {
            // vg-lv (los guiones del nombre se escriben dobles)
            let m = m.replace("--", "\u{0}");
            m.split_once('-').map(|(vg, lv)| format!("{}/{}", vg.replace('\u{0}', "-"), lv.replace('\u{0}', "-")).to_lowercase())
        } else {
            dev.strip_prefix("/dev/").map(|d| format!("{}/{}", d.split('/').next().unwrap_or(""), d.split('/').nth(1).unwrap_or("")).to_lowercase())
        };
        if let Some(id) = id {
            out.insert(id, mnt.to_string());
        }
    }
    out
}

#[tauri::command(async)]
fn open_extraction(state: State<AppState>, dir: String) -> CmdResult<ExtractionInfo> {
    let p = PathBuf::from(&dir);
    let disks = volumes::open_source(&p).map_err(|e| format!("{e:#}"))?;
    let vols = volumes::scan(&disks).map_err(|e| format!("{e:#}"))?;
    let fstab = read_fstab(&disks, &vols);
    let mut dtos: Vec<VolDto> = vols.iter().map(|v| friendly(&disks, v, &fstab)).collect();
    dtos.sort_by(|a, b| b.browsable.cmp(&a.browsable).then(b.size.cmp(&a.size)));
    let conf = std::fs::read_to_string(p.join("qemu-server.conf")).unwrap_or_default();
    let name = conf_value(&conf, "name").map(String::from).unwrap_or_else(|| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    let os = os_family(conf_value(&conf, "ostype").unwrap_or("")).to_string();
    let disk_usage = disks.iter().filter_map(|d| sparse::allocated_size(&d.path)).sum();
    *state.explorer.lock().unwrap() = Some(Explorer { dir: p, vols });
    Ok(ExtractionInfo { dir, name, os, vols: dtos, disk_usage })
}

fn with_volume<T>(state: &AppState, vol: &str, f: impl FnOnce(&[volumes::Disk], &Volume) -> anyhow::Result<T>) -> CmdResult<T> {
    let (dir, v) = {
        let g = state.explorer.lock().unwrap();
        let ex = g.as_ref().ok_or("no hay ninguna extracción abierta")?;
        (ex.dir.clone(), volumes::find(&ex.vols, vol).map_err(err)?.clone())
    };
    let disks = volumes::open_source(&dir).map_err(|e| format!("{e:#}"))?;
    f(&disks, &v).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn list_dir(state: State<AppState>, vol: String, path: String) -> CmdResult<Vec<fs::Entry>> {
    with_volume(&state, &vol, |disks, v| fs::open(disks, v)?.list(&fs::components(&path)))
}

#[derive(Serialize, Clone)]
struct CopyResult {
    stats: fs::CopyStats,
    dest: String,
}

#[tauri::command]
fn start_copy(app: AppHandle, state: State<AppState>, vol: String, paths: Vec<String>, dest: String) -> CmdResult<u64> {
    let (dir, v) = {
        let g = state.explorer.lock().unwrap();
        let ex = g.as_ref().ok_or("no hay ninguna extracción abierta")?;
        (ex.dir.clone(), volumes::find(&ex.vols, &vol).map_err(err)?.clone())
    };
    let (id, cancel) = new_job(&state);
    std::thread::spawn(move || {
        let res = (|| -> anyhow::Result<CopyResult> {
            let disks = volumes::open_source(&dir)?;
            let mut f = fs::open(&disks, &v)?;
            let emit = |fraction: f64, done: u64, total: u64, rate: u64, eta: Option<u64>, text: String| {
                let _ = app.emit(
                    "job-progress",
                    JobProgress { id, kind: "copy".into(), fraction, eta_secs: eta, bytes_per_sec: rate, done_bytes: done, total_bytes: total, text },
                );
            };
            emit(0.0, 0, 0, 0, None, "Calculando tamaño…".into());
            let (mut files_total, mut bytes_total) = (0, 0);
            for p in &paths {
                let (n, b) = f.total_size(&fs::components(p), &cancel);
                files_total += n;
                bytes_total += b;
            }
            let started = Instant::now();
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut done = 0u64;
            let files_done = std::cell::Cell::new(0u64);
            let cur = std::cell::RefCell::new(String::new());
            let mut total = fs::CopyStats::default();
            let dest_path = PathBuf::from(&dest);
            for p in &paths {
                let mut on_file = |name: &str| {
                    files_done.set(files_done.get() + 1);
                    *cur.borrow_mut() = name.to_string();
                };
                let mut on_bytes = |n: u64| {
                    done += n;
                    if last.elapsed() > Duration::from_millis(150) {
                        last = Instant::now();
                        let secs = started.elapsed().as_secs_f64().max(0.001);
                        let rate = done as f64 / secs;
                        let eta = (rate > 0.0 && secs > 1.5).then(|| (bytes_total.saturating_sub(done) as f64 / rate) as u64);
                        let frac = if bytes_total > 0 { (done as f64 / bytes_total as f64).min(1.0) } else { 0.0 };
                        emit(frac, done, bytes_total, rate as u64, eta, format!("{} de {files_total} archivos · {}", files_done.get(), cur.borrow()));
                    }
                };
                let mut ctx = fs::CopyCtx { on_file: &mut on_file, on_bytes: &mut on_bytes, cancel: &cancel };
                let st = f.get(&fs::components(p), &dest_path, &mut ctx)?;
                total.files += st.files;
                total.dirs += st.dirs;
                total.bytes += st.bytes;
                total.skipped += st.skipped;
                total.errors.extend(st.errors);
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
            }
            Ok(CopyResult { stats: total, dest })
        })();
        let cancelled = cancel.load(Ordering::Relaxed);
        finish_job(&app, id, "copy", res, cancelled);
    });
    Ok(id)
}

/// Borra lo que crea la extracción (discos .raw, config e informe) y la carpeta si queda vacía.
fn delete_extraction(dir: &Path) -> u64 {
    let mut freed = 0;
    if let Ok(it) = std::fs::read_dir(dir) {
        for e in it.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let ours = (n.starts_with("disk-") && n.ends_with(".raw")) || n == "extract-report.txt" || n == "qemu-server.conf" || n == "qemu-server.fw";
            if ours {
                freed += sparse::allocated_size(&e.path()).unwrap_or(0);
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir); // solo si está vacía
    freed
}

#[tauri::command(async)]
fn close_extraction(state: State<AppState>, delete: bool) -> u64 {
    let ex = state.explorer.lock().unwrap().take();
    match ex {
        Some(ex) if delete => delete_extraction(&ex.dir),
        _ => 0,
    }
}

#[tauri::command(async)]
fn quit_app(app: AppHandle, state: State<AppState>, delete: bool) {
    for f in state.jobs.lock().unwrap().values() {
        f.store(true, Ordering::Relaxed);
    }
    if let Some(ex) = state.explorer.lock().unwrap().take()
        && delete
    {
        delete_extraction(&ex.dir);
    }
    state.close_ok.store(true, Ordering::Relaxed);
    app.exit(0);
}

// ------------------------------------------------------------------ diálogos

#[tauri::command(async)]
fn pick_backup(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .set_title("Abrir backup de Proxmox")
        .add_filter("Backups de Proxmox", &["zst", "vma", "gz"])
        .add_filter("Todos los archivos", &["*"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command(async)]
fn pick_folder(app: AppHandle, title: String, start: Option<String>) -> Option<String> {
    let mut d = app.dialog().file().set_title(title);
    if let Some(s) = start {
        let mut p = PathBuf::from(s);
        while !p.exists() {
            match p.parent() {
                Some(pp) => p = pp.to_path_buf(),
                None => break,
            }
        }
        d = d.set_directory(p);
    }
    d.blocking_pick_folder().and_then(|f| f.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn reveal(path: String) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(&path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&path).spawn();
}

// ------------------------------------------------------------------ recientes

#[derive(Serialize, serde::Deserialize, Clone)]
struct Recent {
    path: String,
    name: String,
    vmid: Option<String>,
    os: String,
    created: i64,
    file_size: u64,
}

fn recents_file(app: &AppHandle) -> Option<PathBuf> {
    let d = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&d).ok()?;
    Some(d.join("recientes.json"))
}

fn load_recents(app: &AppHandle) -> Vec<Recent> {
    recents_file(app).and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn add_recent(app: &AppHandle, b: &BackupInfo) {
    let mut list = load_recents(app);
    list.retain(|r| r.path != b.path);
    list.insert(0, Recent { path: b.path.clone(), name: b.name.clone(), vmid: b.vmid.clone(), os: b.os.clone(), created: b.created, file_size: b.file_size });
    list.truncate(8);
    if let Some(f) = recents_file(app) {
        let _ = std::fs::write(f, serde_json::to_vec_pretty(&list).unwrap_or_default());
    }
}

#[tauri::command(async)]
fn get_recents(app: AppHandle) -> Vec<Recent> {
    load_recents(&app).into_iter().filter(|r| Path::new(&r.path).exists()).collect()
}

#[tauri::command(async)]
fn is_dir(path: String) -> bool {
    Path::new(&path).is_dir()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if !state.close_ok.load(Ordering::Relaxed) && state.explorer.lock().unwrap().is_some() {
                    api.prevent_close();
                    let _ = window.emit("close-requested", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            backup_info,
            space_info,
            start_analyze,
            start_extract,
            cancel_job,
            open_extraction,
            list_dir,
            start_copy,
            close_extraction,
            quit_app,
            pick_backup,
            pick_folder,
            reveal,
            get_recents,
            is_dir
        ])
        .run(tauri::generate_context!())
        .expect("error al iniciar la aplicación");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &str) -> Vec<String> {
        let disks = volumes::open_source(Path::new(dir)).unwrap();
        let vols = volumes::scan(&disks).unwrap();
        let fstab = read_fstab(&disks, &vols);
        vols.iter().map(|v| { let d = friendly(&disks, v, &fstab); format!("{} -> {} [{}] navegable={}", v.id, d.label, d.detail, d.browsable) }).collect()
    }

    #[test]
    fn nombres_volumenes_reales() {
        for dir in ["../extraido5000", "../extraido407"] {
            if Path::new(dir).exists() {
                println!("{dir}:\n  {}", names(dir).join("\n  "));
            }
        }
        let log = vma::data_size_from_log(Path::new("../vzdump-qemu-407-2026_09_09-16_09_14.vma.zst"));
        println!("log 407: {log:?}");
    }
}
