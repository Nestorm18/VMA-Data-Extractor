//! Navegación de sistemas de archivos ext2/3/4 y NTFS (solo lectura) y copia de
//! archivos/carpetas al disco local.

use anyhow::{Context, Result, anyhow, bail};
use ntfs::indexes::NtfsFileNameIndex;
use ntfs::structured_values::{NtfsFileAttributeFlags, NtfsFileNamespace};
use ntfs::{Ntfs, NtfsFile};
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use crate::volumes::{Disk, FsKind, VolReader, Volume};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// segundos desde 1970
    pub mtime: Option<i64>,
}

pub enum Fs {
    Ext(ext4_view::Ext4),
    Ntfs { ntfs: Ntfs, r: VolReader },
}

#[derive(Default, Clone, Debug, serde::Serialize)]
pub struct CopyStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
}

pub fn open(disks: &[Disk], v: &Volume) -> Result<Fs> {
    if !v.kind.browsable() {
        bail!("el volumen {} es {}: no se puede navegar", v.id, v.kind.label());
    }
    let r = VolReader::new(disks, &v.segs, v.size)?;
    match v.kind {
        FsKind::Ext => Ok(Fs::Ext(ext4_view::Ext4::load(Box::new(r)).map_err(|e| anyhow!("ext4: {e}"))?)),
        FsKind::Ntfs => {
            let mut r = r;
            let mut ntfs = Ntfs::new(&mut r).map_err(|e| anyhow!("NTFS: {e}"))?;
            ntfs.read_upcase_table(&mut r).map_err(|e| anyhow!("NTFS: {e}"))?;
            Ok(Fs::Ntfs { ntfs, r })
        }
        _ => unreachable!(),
    }
}

/// Divide una ruta en componentes, aceptando '/' y '\', y resolviendo '.' y '..'.
pub fn components(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c.to_string()),
        }
    }
    out
}

pub fn join(comps: &[String]) -> String {
    format!("/{}", comps.join("/"))
}

const NTFS_META: &[&str] = &["$MFT", "$MFTMirr", "$LogFile", "$Volume", "$AttrDef", "$Bitmap", "$Boot", "$BadClus", "$Secure", "$UpCase", "$Extend"];

fn ntfs_time(t: ntfs::NtfsTime) -> Option<i64> {
    let ts = t.nt_timestamp();
    if ts == 0 {
        return None;
    }
    Some((ts / 10_000_000) as i64 - 11_644_473_600)
}

fn ntfs_find<'n>(ntfs: &'n Ntfs, r: &mut VolReader, comps: &[String]) -> Result<NtfsFile<'n>> {
    let mut cur = ntfs.root_directory(r)?;
    for (i, c) in comps.iter().enumerate() {
        let next = {
            if !cur.is_directory() {
                bail!("{} no es una carpeta", join(&comps[..i]));
            }
            let index = cur.directory_index(r)?;
            let mut finder = index.finder();
            let entry = NtfsFileNameIndex::find(&mut finder, ntfs, r, c).ok_or_else(|| anyhow!("no existe: {}", join(&comps[..=i])))??;
            entry.to_file(ntfs, r)?
        };
        cur = next;
    }
    Ok(cur)
}

fn ntfs_list(ntfs: &Ntfs, r: &mut VolReader, comps: &[String]) -> Result<Vec<Entry>> {
    let dir = ntfs_find(ntfs, r, comps)?;
    let index = dir.directory_index(r)?;
    let mut it = index.entries();
    let mut out = Vec::new();
    while let Some(e) = it.next(r) {
        let e = e?;
        let Some(key) = e.key() else { continue };
        let key = key?;
        if key.namespace() == NtfsFileNamespace::Dos {
            continue;
        }
        let name = key.name().to_string_lossy();
        if name == "." {
            continue;
        }
        let attrs = key.file_attributes();
        out.push(Entry {
            name,
            is_dir: key.is_directory(),
            is_link: attrs.contains(NtfsFileAttributeFlags::REPARSE_POINT),
            size: key.data_size(),
            mtime: ntfs_time(key.modification_time()),
        });
    }
    Ok(out)
}

impl Fs {
    pub fn list(&mut self, comps: &[String]) -> Result<Vec<Entry>> {
        let mut v = match self {
            Fs::Ext(fs) => {
                let mut out = Vec::new();
                for e in fs.read_dir(join(comps).as_str()).map_err(|e| anyhow!("{}: {e}", join(comps)))? {
                    let e = e.map_err(|e| anyhow!("{e}"))?;
                    let name = e.file_name().display().to_string();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let ft = e.file_type().map_err(|e| anyhow!("{e}"))?;
                    let md = e.metadata().map_err(|e| anyhow!("{e}"))?;
                    out.push(Entry {
                        name,
                        is_dir: ft.is_dir(),
                        is_link: ft.is_symlink(),
                        size: if ft.is_regular_file() { md.len() } else { 0 },
                        mtime: Some(md.modified().seconds()),
                    });
                }
                out
            }
            Fs::Ntfs { ntfs, r } => ntfs_list(ntfs, r, comps)?,
        };
        v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(v)
    }

    pub fn is_dir(&mut self, comps: &[String]) -> Result<bool> {
        if comps.is_empty() {
            return Ok(true);
        }
        match self {
            Fs::Ext(fs) => Ok(fs.metadata(join(comps).as_str()).map_err(|e| anyhow!("{}: {e}", join(comps)))?.is_dir()),
            Fs::Ntfs { ntfs, r } => Ok(ntfs_find(ntfs, r, comps)?.is_directory()),
        }
    }

    /// Copia un archivo; devuelve los bytes copiados.
    fn copy_file(&mut self, comps: &[String], dest: &Path, mtime: Option<i64>, ctx: &mut CopyCtx) -> Result<u64> {
        let out = File::create(dest).with_context(|| format!("no se puede crear {}", dest.display()))?;
        let mut w = BufWriter::with_capacity(1 << 20, out);
        let n = match self {
            Fs::Ext(fs) => {
                let mut f = fs.open(join(comps).as_str()).map_err(|e| anyhow!("{e}"))?;
                copy_stream(&mut f, &mut w, ctx)?
            }
            Fs::Ntfs { ntfs, r } => {
                let file = ntfs_find(ntfs, r, comps)?;
                match file.data(r, "") {
                    None => 0,
                    Some(item) => {
                        let item = item?;
                        let attr = item.to_attribute()?;
                        let value = attr.value(r)?;
                        let mut rd = value.attach(r);
                        copy_stream(&mut rd, &mut w, ctx)?
                    }
                }
            }
        };
        let f = w.into_inner().map_err(|e| e.into_error())?;
        if let Some(t) = mtime
            && t >= 0
        {
            let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(t as u64));
        }
        Ok(n)
    }

    /// Cuenta archivos y bytes de una ruta (para la barra de progreso).
    pub fn total_size(&mut self, comps: &[String], cancel: &AtomicBool) -> (u64, u64) {
        if !self.is_dir(comps).unwrap_or(false) {
            return (1, self.entry_of(comps).map(|e| e.size).unwrap_or(0));
        }
        let mut files = 0;
        let mut bytes = 0;
        let mut stack = vec![comps.to_vec()];
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(entries) = self.list(&dir) else { continue };
            for e in entries {
                if e.is_link || (dir.is_empty() && NTFS_META.contains(&e.name.as_str())) {
                    continue;
                }
                let mut child = dir.clone();
                child.push(e.name);
                if e.is_dir {
                    stack.push(child);
                } else {
                    files += 1;
                    bytes += e.size;
                }
            }
        }
        (files, bytes)
    }

    /// Copia un archivo o una carpeta (recursivamente) a `dest_dir`.
    pub fn get(&mut self, comps: &[String], dest_dir: &Path, ctx: &mut CopyCtx) -> Result<CopyStats> {
        let mut st = CopyStats::default();
        std::fs::create_dir_all(dest_dir)?;
        let name = comps.last().cloned().unwrap_or_else(|| "raiz".into());
        let target = dest_dir.join(safe_name(&name));
        if self.is_dir(comps)? {
            self.copy_dir(comps, &target, &mut st, ctx);
        } else {
            let mtime = self.entry_of(comps).ok().and_then(|e| e.mtime);
            self.copy_one(comps, &target, mtime, &mut st, ctx);
        }
        Ok(st)
    }

    fn copy_one(&mut self, comps: &[String], dest: &Path, mtime: Option<i64>, st: &mut CopyStats, ctx: &mut CopyCtx) {
        (ctx.on_file)(&join(comps));
        match self.copy_file(comps, dest, mtime, ctx) {
            Ok(n) => {
                st.files += 1;
                st.bytes += n;
            }
            Err(e) => {
                if !ctx.cancel.load(Ordering::Relaxed) {
                    st.errors.push(format!("{}: {e:#}", join(comps)));
                }
                let _ = std::fs::remove_file(dest);
            }
        }
    }

    fn entry_of(&mut self, comps: &[String]) -> Result<Entry> {
        let (parent, name) = comps.split_at(comps.len().saturating_sub(1));
        let name = name.first().context("ruta vacía")?;
        self.list(parent)?.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)).context("no encontrado")
    }

    fn copy_dir(&mut self, comps: &[String], target: &Path, st: &mut CopyStats, ctx: &mut CopyCtx) {
        if ctx.cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = std::fs::create_dir_all(target) {
            st.errors.push(format!("{}: {e}", target.display()));
            return;
        }
        st.dirs += 1;
        let entries = match self.list(comps) {
            Ok(v) => v,
            Err(e) => {
                st.errors.push(format!("{}: {e:#}", join(comps)));
                return;
            }
        };
        let is_ntfs = matches!(self, Fs::Ntfs { .. });
        for e in entries {
            if ctx.cancel.load(Ordering::Relaxed) {
                return;
            }
            if is_ntfs && comps.is_empty() && NTFS_META.contains(&e.name.as_str()) {
                continue;
            }
            let mut child = comps.to_vec();
            child.push(e.name.clone());
            let dest = target.join(safe_name(&e.name));
            if e.is_link {
                st.skipped += 1;
                continue;
            }
            if e.is_dir {
                self.copy_dir(&child, &dest, st, ctx);
            } else {
                self.copy_one(&child, &dest, e.mtime, st, ctx);
            }
        }
    }
}

/// Callbacks y cancelación de una copia.
pub struct CopyCtx<'a> {
    /// Se llama al empezar cada archivo (ruta dentro del volumen).
    pub on_file: &'a mut dyn FnMut(&str),
    /// Se llama con los bytes copiados de cada trozo.
    pub on_bytes: &'a mut dyn FnMut(u64),
    pub cancel: &'a AtomicBool,
}

fn copy_stream(rd: &mut impl Read, w: &mut impl Write, ctx: &mut CopyCtx) -> io::Result<u64> {
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0;
    loop {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelado"));
        }
        let n = rd.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        w.write_all(&buf[..n])?;
        total += n as u64;
        (ctx.on_bytes)(n as u64);
    }
}

/// Nombre válido en el sistema local (Windows no admite <>:"/\|?* ni CON, NUL...).
pub fn safe_name(name: &str) -> PathBuf {
    let mut s: String = name
        .chars()
        .map(|c| if c < ' ' || (cfg!(windows) && "<>:\"/\\|?*".contains(c)) || c == '/' { '_' } else { c })
        .collect();
    if cfg!(windows) {
        while s.ends_with('.') || s.ends_with(' ') {
            s.pop();
            s.push('_');
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            s.insert(0, '_');
        }
    }
    if s.is_empty() {
        s.push('_');
    }
    PathBuf::from(s)
}

pub fn fmt_time(t: Option<i64>) -> String {
    let Some(t) = t else { return "                ".into() };
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // algoritmo de Howard Hinnant (días -> fecha civil)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}
