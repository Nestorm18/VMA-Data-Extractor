use vmatool::{fs, human, vma, volumes};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Extrae y explora backups de Proxmox (vzdump .vma / .vma.zst / .vma.gz) sin restaurarlos.
#[derive(Parser)]
#[command(name = "vmatool", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Muestra la configuración y los discos del backup (rápido)
    Info { backup: PathBuf },
    /// Lee el backup entero y comprueba el checksum
    Verify { backup: PathBuf },
    /// Extrae los discos a archivos .raw dispersos
    Extract {
        backup: PathBuf,
        /// Carpeta de destino
        outdir: PathBuf,
        /// Ignora el checksum de zstd para recuperar un backup dañado
        #[arg(long)]
        ignore_checksum: bool,
        /// Sobrescribe archivos .raw existentes
        #[arg(long)]
        force: bool,
    },
    /// Lista particiones, volúmenes LVM y sistemas de archivos de los discos extraídos
    Vols {
        /// Archivo .raw o carpeta de extracción
        source: PathBuf,
    },
    /// Lista una carpeta dentro de un volumen
    Ls {
        source: PathBuf,
        /// Volumen (id o número de 'vols')
        vol: String,
        #[arg(default_value = "/")]
        path: String,
    },
    /// Copia un archivo o carpeta del volumen al disco local
    Get {
        source: PathBuf,
        vol: String,
        path: String,
        /// Carpeta local de destino
        #[arg(default_value = ".")]
        dest: PathBuf,
    },
    /// Explorador interactivo (cd, ls, get...)
    Shell {
        source: PathBuf,
        vol: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Info { backup } => {
            let mut s = vma::open(&backup, false)?;
            let h = vma::read_header(&mut s)?;
            println!("VMA versión {}, creado {} (UTC)", h.version, fs::fmt_time(Some(h.ctime)));
            for (name, data) in &h.configs {
                println!("\n--- {name} ---\n{}", String::from_utf8_lossy(data).trim_end());
            }
            println!("\nDiscos:");
            for d in &h.devices {
                println!("  {:<20} {:>12}  -> {}", d.name, human(d.size), vma::disk_file_name(&d.name));
            }
        }
        Cmd::Verify { backup } => return verify(&backup),
        Cmd::Extract { backup, outdir, ignore_checksum, force } => return extract(&backup, &outdir, ignore_checksum, force),
        Cmd::Vols { source } => {
            let disks = volumes::open_source(&source)?;
            print_vols(&volumes::scan(&disks)?);
        }
        Cmd::Ls { source, vol, path } => {
            let disks = volumes::open_source(&source)?;
            let vols = volumes::scan(&disks)?;
            let mut f = fs::open(&disks, volumes::find(&vols, &vol)?)?;
            print_list(&f.list(&fs::components(&path))?);
        }
        Cmd::Get { source, vol, path, dest } => {
            let disks = volumes::open_source(&source)?;
            let vols = volumes::scan(&disks)?;
            let mut f = fs::open(&disks, volumes::find(&vols, &vol)?)?;
            return Ok(if do_get(&mut f, &fs::components(&path), &dest)? { 0 } else { 2 });
        }
        Cmd::Shell { source, vol } => shell(&source, vol)?,
    }
    Ok(0)
}

fn print_vols(vols: &[volumes::Volume]) {
    println!("{:>3}  {:<28} {:>12}  {:<20} descripción", "nº", "volumen", "tamaño", "tipo");
    for (i, v) in vols.iter().enumerate() {
        println!("{:>3}  {:<28} {:>12}  {:<20} {}", i + 1, v.id, human(v.size), v.kind.label(), v.desc);
    }
}

fn print_list(entries: &[fs::Entry]) {
    for e in entries {
        let kind = if e.is_link { "<LINK>" } else if e.is_dir { "<DIR>" } else { "" };
        let size = if e.is_dir || e.is_link { String::new() } else { human(e.size) };
        println!("{}  {:>6} {:>12}  {}", fs::fmt_time(e.mtime), kind, size, e.name);
    }
    println!("{} elementos", entries.len());
}

fn print_progress(p: &vma::Progress) {
    let eta = p.eta_secs.map(|s| format!(" · quedan {}:{:02}", s / 60, s % 60)).unwrap_or_default();
    eprint!(
        "\r  {:5.1}%  {} de {}  ({}/s){eta}      ",
        p.fraction * 100.0,
        human(p.input_read),
        human(p.input_size),
        human(p.bytes_per_sec)
    );
}

fn verify(backup: &Path) -> Result<i32> {
    let r = vma::scan_with(backup, print_progress, &AtomicBool::new(false))?;
    eprintln!();
    for d in &r.disks {
        println!("{}: disco {} | datos {}", d.name, human(d.size), human(d.data_bytes));
    }
    for e in &r.errors {
        println!("ERROR: {e}");
    }
    let ok = r.complete && r.errors.is_empty();
    println!("{} extents en {} s. Resultado: {}", r.extents, r.elapsed_secs, if ok { "CORRECTO (checksum OK)" } else { "DAÑADO" });
    Ok(if ok { 0 } else { 2 })
}

fn extract(backup: &Path, outdir: &Path, ignore_checksum: bool, force: bool) -> Result<i32> {
    let started = Instant::now();
    let r = vma::extract_with(backup, outdir, ignore_checksum, force, print_progress, &AtomicBool::new(false))?;
    eprintln!();
    print!("{}", r.to_text(backup));
    if !r.complete {
        if r.checksum_error && !ignore_checksum {
            eprintln!("
El checksum no coincide. Repite con --ignore-checksum para sacar todo lo posible.");
        }
        eprintln!("
AVISO: extracción incompleta. Lo extraído sigue en {}", outdir.display());
        return Ok(2);
    }
    eprintln!("
Extracción terminada en {:.0?}.", started.elapsed());
    Ok(0)
}

fn do_get(f: &mut fs::Fs, comps: &[String], dest: &Path) -> Result<bool> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut on_file = |_: &str| {
        files += 1;
        if files % 200 == 0 {
            eprint!("\r  {files} archivos...      ");
        }
    };
    let mut on_bytes = |n: u64| bytes += n;
    let cancel = AtomicBool::new(false);
    let mut ctx = fs::CopyCtx { on_file: &mut on_file, on_bytes: &mut on_bytes, cancel: &cancel };
    let st = f.get(comps, dest, &mut ctx)?;
    eprintln!("\r{:60}", "");
    println!("Copiados {} archivos ({}) y {} carpetas a {}", st.files, human(st.bytes), st.dirs, dest.display());
    if st.skipped > 0 {
        println!("{} enlaces simbólicos/puntos de unión omitidos", st.skipped);
    }
    for e in st.errors.iter().take(50) {
        println!("ERROR: {e}");
    }
    if st.errors.len() > 50 {
        println!("... y {} errores más", st.errors.len() - 50);
    }
    Ok(st.errors.is_empty())
}

const HELP: &str = "\
Comandos:
  vols                 lista los volúmenes
  use <vol>            abre un volumen (id o número)
  ls [ruta]            lista la carpeta
  cd <ruta>            cambia de carpeta ('..' para subir)
  pwd                  muestra la carpeta actual
  get <ruta> [destino] copia un archivo o carpeta al PC (destino por defecto: carpeta actual)
  help                 esta ayuda
  exit                 salir";

fn shell(source: &Path, vol: Option<String>) -> Result<()> {
    let disks = volumes::open_source(source)?;
    let vols = volumes::scan(&disks)?;
    print_vols(&vols);
    let mut cur: Option<(String, fs::Fs)> = None;
    let mut cwd: Vec<String> = Vec::new();

    let open_vol = |id: &str| -> Result<(String, fs::Fs)> {
        let v = volumes::find(&vols, id)?;
        Ok((v.id.clone(), fs::open(&disks, v)?))
    };
    let first = vol.or_else(|| vols.iter().find(|v| v.kind.browsable()).map(|v| v.id.clone()));
    if let Some(id) = first {
        match open_vol(&id) {
            Ok(v) => {
                println!("\nVolumen abierto: {}", v.0);
                cur = Some(v);
            }
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    println!("Escribe 'help' para ver los comandos.");

    let stdin = std::io::stdin();
    loop {
        let prompt = match &cur {
            Some((id, _)) => format!("{id}:{}> ", fs::join(&cwd)),
            None => "> ".into(),
        };
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let args = split_args(line.trim());
        let Some(cmd) = args.first().map(|s| s.as_str()) else { continue };
        let arg = |i: usize| args.get(i).map(|s| s.as_str());
        let base = fs::join(&cwd);
        let resolve = |p: &str| {
            if p.starts_with('/') || p.starts_with('\\') { fs::components(p) } else { fs::components(&format!("{base}/{p}")) }
        };
        let res: Result<()> = (|| {
            match cmd {
                "exit" | "quit" | "salir" => return Err(anyhow::anyhow!("__exit")),
                "help" | "?" | "ayuda" => println!("{HELP}"),
                "vols" => print_vols(&vols),
                "use" => {
                    let id = arg(1).ok_or_else(|| anyhow::anyhow!("uso: use <vol>"))?;
                    cur = Some(open_vol(id)?);
                    cwd.clear();
                }
                "pwd" => println!("{}", fs::join(&cwd)),
                "ls" | "dir" => {
                    let (_, f) = cur.as_mut().ok_or_else(|| anyhow::anyhow!("abre un volumen con 'use'"))?;
                    print_list(&f.list(&resolve(arg(1).unwrap_or(".")))?);
                }
                "cd" => {
                    let (_, f) = cur.as_mut().ok_or_else(|| anyhow::anyhow!("abre un volumen con 'use'"))?;
                    let target = resolve(arg(1).unwrap_or("/"));
                    if !f.is_dir(&target)? {
                        anyhow::bail!("no es una carpeta");
                    }
                    cwd = target;
                }
                "get" => {
                    let (_, f) = cur.as_mut().ok_or_else(|| anyhow::anyhow!("abre un volumen con 'use'"))?;
                    let p = arg(1).ok_or_else(|| anyhow::anyhow!("uso: get <ruta> [destino]"))?;
                    let dest = PathBuf::from(arg(2).unwrap_or("."));
                    do_get(f, &resolve(p), &dest)?;
                }
                other => println!("comando desconocido: {other} (escribe 'help')"),
            }
            Ok(())
        })();
        if let Err(e) = res {
            if e.to_string() == "__exit" {
                break;
            }
            eprintln!("error: {e:#}");
        }
    }
    Ok(())
}

/// Separa argumentos respetando comillas ("Program Files").
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}
