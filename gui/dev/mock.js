// Simulación de window.__TAURI__ para probar la interfaz en un navegador.
window.MOCK_NOLOG = location.search.includes("nolog"); window.MOCK_BAD = location.search.includes("bad");
(() => {
  const listeners = {};
  const emit = (ev, payload) => (listeners[ev] || []).forEach((f) => f({ payload }));
  let job = 0;
  const cancels = {};
  function fakeJob(kind, total, ms, result) {
    const id = ++job;
    cancels[id] = false;
    let t = 0;
    const timer = setInterval(() => {
      t += 100;
      const frac = Math.min(t / ms, 1);
      emit("job-progress", { id, kind, fraction: frac, eta_secs: Math.round((ms - t) / 1000), bytes_per_sec: 180e6, done_bytes: total * frac, total_bytes: total, text: kind === "copy" ? `12 de 40 archivos · /Users/Administrador/Documents/factura-${t}.pdf` : "" });
      if (cancels[id] || frac >= 1) {
        clearInterval(timer);
        emit("job-done", { id, kind, ok: !cancels[id], cancelled: cancels[id], error: null, result: result() });
      }
    }, 100);
    return id;
  }
  const backup = {
    path: "C:\\b\\vzdump-qemu-407-2026_09_09-16_09_14.vma.zst", file_name: "vzdump-qemu-407-2026_09_09-16_09_14.vma.zst", file_size: 25763154244,
    vmid: "407", name: "Win25Tubanor", os: "windows", cores: 4, memory_mb: 8192, created: 1757426954,
    disks: [{ name: "efidisk0", size: 540672 }, { name: "scsi0", size: 107374182400 }, { name: "scsi1", size: 214748364800 }, { name: "tpmstate0-backup", size: 4194304 }],
    total_disk_size: 322127282176, log_data_bytes: window.MOCK_NOLOG ? null : 31097345720, suggested_outdir: "C:\\b\\Win25Tubanor-407", config: "",
  };
  const report = { complete: true, cancelled: false, extents: 84747, skipped_bytes: 0, errors: window.MOCK_BAD ? ["error de lectura: Restored data doesn't match checksum"] : [], checksum_error: !!window.MOCK_BAD, elapsed_secs: 155,
    disks: [{ name: "drive-efidisk0", file: "", size: 540672, data_bytes: 540672 }, { name: "drive-scsi0", file: "", size: 107374182400, data_bytes: 31096000000 }, { name: "drive-scsi1", file: "", size: 214748364800, data_bytes: 4096 }, { name: "drive-tpmstate0-backup", file: "", size: 4194304, data_bytes: 16384 }] };
  const now = 1747000000;
  const dirs = {
    "/": [["$MFT", 0, 363331584], ["Program Files", 1], ["ProgramData", 1], ["Users", 1], ["Windows", 1], ["pagefile.sys", 0, 1342177280], ["Documents and Settings", 1, 0, true]],
    "/Users": [["Administrador", 1], ["Public", 1], ["Default", 1], ["All Users", 1, 0, true], ["desktop.ini", 0, 174]],
    "/Users/Administrador": [["Desktop", 1], ["Documents", 1], ["Downloads", 1], ["NTUSER.DAT", 0, 2202009]],
  };
  const commands = {
    get_recents: () => [{ path: backup.path, name: "Win25Tubanor", vmid: "407", os: "windows", created: 1757426954, file_size: 25763154244 }, { path: "C:\\b\\vzdump-qemu-5000.vma.zst", name: "proxdopy", vmid: "5000", os: "linux", created: 1762194065, file_size: 850691660 }],
    is_dir: ({ path }) => !path.endsWith(".zst"),
    backup_info: () => backup,
    space_info: ({ outdir }) => ({ free: outdir.startsWith("D") ? 900e9 : 30e9, sparse: !outdir.startsWith("E"), existing: outdir.endsWith("existe") }),
    start_analyze: () => fakeJob("analyze", 25763154244, 2500, () => report),
    start_extract: () => fakeJob("extract", 25763154244, 3000, () => report),
    cancel_job: ({ id }) => (cancels[id] = true),
    open_extraction: ({ dir }) => ({ dir, name: "Win25Tubanor", os: "windows", disk_usage: 31097345720, vols: [
      { id: "scsi0:3", label: "Windows (C:)", detail: "NTFS · scsi0:3", kind: "NTFS", icon: "windows", size: 106374182400, browsable: true },
      { id: "scsi0:4", label: "Recuperación", detail: "NTFS · scsi0:4", kind: "NTFS", icon: "lifebuoy", size: 872415232, browsable: true },
      { id: "scsi1", label: "scsi1 · vacío", detail: "sin datos", kind: "-", icon: "disk", size: 214748364800, browsable: false },
      { id: "scsi0:1", label: "EFI", detail: "FAT · scsi0:1", kind: "FAT", icon: "bolt", size: 104857600, browsable: false },
      { id: "scsi0:2", label: "Reservada de Microsoft", detail: "-", kind: "-", icon: "disk", size: 16777216, browsable: false } ] }),
    list_dir: ({ path }) => (dirs[path] || []).map(([name, d, size = 0, link = false], i) => ({ name, is_dir: !!d, is_link: link, size, mtime: now - i * 86400 * 20 })),
    start_copy: () => fakeJob("copy", 5e9, 3000, () => ({ stats: { files: 40, dirs: 3, bytes: 5e9, skipped: 1, errors: [] }, dest: "D:\\Recuperado" })),
    pick_backup: () => backup.path,
    pick_folder: ({ title }) => (title.startsWith("Copiar") ? "D:\\Recuperado" : "D:\\Extracciones\\Win25Tubanor"),
    close_extraction: () => 31097345720,
    quit_app: () => console.log("quit"),
    reveal: () => {},
  };
  window.__TAURI__ = {
    core: { invoke: async (cmd, args = {}) => { await new Promise((r) => setTimeout(r, 60)); if (!commands[cmd]) throw "sin mock: " + cmd; return commands[cmd](args); } },
    event: { listen: async (ev, f) => { (listeners[ev] ||= []).push(f); return () => {}; } },
    app: { getVersion: async () => "1.0.0" },
    webview: { getCurrentWebview: () => ({ onDragDropEvent: () => {} }) },
  };
})();
