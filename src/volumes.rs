//! Discos extraídos (.raw), tablas de particiones (MBR/GPT), LVM y detección de
//! sistemas de archivos.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::lvm;

pub struct Disk {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// `source` puede ser un .raw o la carpeta de extracción (se usan todos sus .raw).
pub fn open_source(source: &Path) -> Result<Vec<Disk>> {
    let mut paths = Vec::new();
    if source.is_dir() {
        for e in std::fs::read_dir(source)? {
            let p = e?.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if p.is_file() && (ext == "raw" || ext == "img") {
                paths.push(p);
            }
        }
        paths.sort();
        if paths.is_empty() {
            bail!("no hay archivos .raw en {}", source.display());
        }
    } else {
        paths.push(source.to_path_buf());
    }
    let mut disks = Vec::new();
    for p in paths {
        let size = std::fs::metadata(&p).with_context(|| format!("no se puede abrir {}", p.display()))?.len();
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("disk").to_string();
        let name = stem.trim_start_matches("disk-").trim_start_matches("drive-").to_string();
        disks.push(Disk { name, path: p, size });
    }
    Ok(disks)
}

#[derive(Clone, Debug)]
pub struct Seg {
    pub start: u64,
    pub len: u64,
    pub disk: usize,
    pub disk_off: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Ext,
    Ntfs,
    Fat,
    Xfs,
    Btrfs,
    Swap,
    LvmPv,
    Luks,
    BitLocker,
    Unknown,
}

impl FsKind {
    pub fn label(self) -> &'static str {
        match self {
            FsKind::Ext => "ext2/3/4",
            FsKind::Ntfs => "NTFS",
            FsKind::Fat => "FAT",
            FsKind::Xfs => "XFS (no soportado)",
            FsKind::Btrfs => "Btrfs (no soportado)",
            FsKind::Swap => "swap",
            FsKind::LvmPv => "LVM PV",
            FsKind::Luks => "LUKS cifrado",
            FsKind::BitLocker => "BitLocker cifrado",
            FsKind::Unknown => "-",
        }
    }
    pub fn browsable(self) -> bool {
        matches!(self, FsKind::Ext | FsKind::Ntfs)
    }
}

#[derive(Clone)]
pub struct Volume {
    pub id: String,
    pub desc: String,
    pub size: u64,
    pub kind: FsKind,
    pub segs: Vec<Seg>,
}

/// Lector Read+Seek sobre un volumen (partición o LV) repartido en segmentos de disco.
pub struct VolReader {
    files: Vec<File>,
    segs: Vec<Seg>,
    size: u64,
    pos: u64,
}

impl VolReader {
    pub fn new(disks: &[Disk], segs: &[Seg], size: u64) -> Result<Self> {
        let files = disks
            .iter()
            .map(|d| File::open(&d.path).with_context(|| format!("no se puede abrir {}", d.path.display())))
            .collect::<Result<_>>()?;
        Ok(VolReader { files, segs: segs.to_vec(), size, pos: 0 })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn read_at(&mut self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.pos = off;
        let mut got = 0;
        while got < len {
            let n = self.read(&mut buf[got..])?;
            if n == 0 {
                break; // más allá del final: se queda en ceros
            }
            got += n;
        }
        Ok(buf)
    }
}

impl Read for VolReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.size - self.pos) as usize;
        let seg = self.segs.iter().find(|s| self.pos >= s.start && self.pos < s.start + s.len).cloned();
        let n = match seg {
            None => {
                // hueco sin mapear: ceros hasta el siguiente segmento
                let next = self.segs.iter().filter(|s| s.start > self.pos).map(|s| s.start).min().unwrap_or(self.size);
                let n = want.min((next - self.pos) as usize);
                buf[..n].fill(0);
                n
            }
            Some(s) => {
                let n = want.min((s.start + s.len - self.pos) as usize);
                let f = &mut self.files[s.disk];
                f.seek(SeekFrom::Start(s.disk_off + (self.pos - s.start)))?;
                let mut got = 0;
                while got < n {
                    let k = f.read(&mut buf[got..n])?;
                    if k == 0 {
                        buf[got..n].fill(0); // .raw más corto de lo esperado
                        got = n;
                    } else {
                        got += k;
                    }
                }
                n
            }
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for VolReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.size as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek negativo"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

impl ext4_view::Ext4Read for VolReader {
    fn read(&mut self, start_byte: u64, dst: &mut [u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.seek(SeekFrom::Start(start_byte))?;
        self.read_exact(dst)?;
        Ok(())
    }
}

// ------------------------------------------------------------- detección

fn detect(r: &mut VolReader) -> io::Result<FsKind> {
    let b = r.read_at(0, 4096 + 8)?;
    if &b[3..11] == b"NTFS    " {
        return Ok(FsKind::Ntfs);
    }
    if &b[3..11] == b"-FVE-FS-" {
        return Ok(FsKind::BitLocker);
    }
    if &b[1080..1082] == [0x53, 0xEF] {
        return Ok(FsKind::Ext);
    }
    if &b[..4] == b"XFSB" {
        return Ok(FsKind::Xfs);
    }
    if &b[..6] == b"LUKS\xba\xbe" {
        return Ok(FsKind::Luks);
    }
    if (0..4).any(|s| &b[s * 512..s * 512 + 8] == b"LABELONE") {
        return Ok(FsKind::LvmPv);
    }
    if &b[4086..4096] == b"SWAPSPACE2" || &b[4086..4096] == b"SWAP-SPACE" {
        return Ok(FsKind::Swap);
    }
    if b[510] == 0x55 && b[511] == 0xAA && (&b[82..87] == b"FAT32" || &b[54..59] == b"FAT16" || &b[54..59] == b"FAT12") {
        return Ok(FsKind::Fat);
    }
    if r.size() > 0x10048 {
        let bt = r.read_at(0x10040, 8)?;
        if &bt[..] == b"_BHRfS_M" {
            return Ok(FsKind::Btrfs);
        }
    }
    Ok(FsKind::Unknown)
}

// ------------------------------------------------------------- particiones

struct Part {
    num: u32,
    start: u64,
    len: u64,
    desc: String,
}

fn guid_str(g: &[u8]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{}",
        u32::from_le_bytes(g[0..4].try_into().unwrap()),
        u16::from_le_bytes(g[4..6].try_into().unwrap()),
        u16::from_le_bytes(g[6..8].try_into().unwrap()),
        g[8],
        g[9],
        g[10..16].iter().map(|b| format!("{b:02X}")).collect::<String>()
    )
}

fn gpt_type_name(guid: &str) -> &'static str {
    match guid {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reservada",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft datos",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recuperación",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux",
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709" => "Linux raíz",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "Linux RAID",
        _ => "",
    }
}

fn read_disk(f: &mut File, off: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; len];
    f.seek(SeekFrom::Start(off))?;
    let mut got = 0;
    while got < len {
        let n = f.read(&mut b[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok(b)
}

fn partitions(d: &Disk) -> Result<Vec<Part>> {
    let mut f = File::open(&d.path)?;
    let head = read_disk(&mut f, 0, 8192)?;

    for ss in [512u64, 4096] {
        let h = &head[ss as usize..ss as usize + 92];
        if &h[..8] != b"EFI PART" {
            continue;
        }
        let entries_lba = u64::from_le_bytes(h[72..80].try_into().unwrap());
        let count = u32::from_le_bytes(h[80..84].try_into().unwrap()).min(1024) as usize;
        let esz = u32::from_le_bytes(h[84..88].try_into().unwrap()) as usize;
        if esz < 128 {
            bail!("GPT con tamaño de entrada no válido");
        }
        let tbl = read_disk(&mut f, entries_lba * ss, count * esz)?;
        let mut parts = Vec::new();
        for i in 0..count {
            let e = &tbl[i * esz..i * esz + 128];
            if e[..16].iter().all(|&b| b == 0) {
                continue;
            }
            let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
            let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
            let name: Vec<u16> = e[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
            let ty = guid_str(&e[..16]);
            let tn = gpt_type_name(&ty);
            let name = String::from_utf16_lossy(&name);
            let desc = match (tn, name.is_empty()) {
                ("", true) => ty,
                ("", false) => name,
                (t, true) => t.to_string(),
                (t, false) => format!("{t} \"{name}\""),
            };
            parts.push(Part { num: i as u32 + 1, start: first * ss, len: (last + 1 - first) * ss, desc });
        }
        return Ok(parts);
    }

    // MBR (descartando sectores de arranque de un sistema de archivos sin tabla)
    if head[510] != 0x55 || head[511] != 0xAA || &head[3..11] == b"NTFS    " || &head[82..87] == b"FAT32" || &head[54..57] == b"FAT" {
        return Ok(vec![]);
    }
    let entry = |b: &[u8], i: usize| {
        let e = &b[446 + i * 16..446 + i * 16 + 16];
        (e[4], u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64, u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64)
    };
    let mut parts = Vec::new();
    for i in 0..4 {
        let (ty, lba, n) = entry(&head, i);
        if ty == 0 || n == 0 || (lba + n) * 512 > d.size + 512 {
            continue;
        }
        if matches!(ty, 0x05 | 0x0F | 0x85) {
            // particiones lógicas: cadena de EBR
            let mut ebr = lba;
            let mut num = 5;
            for _ in 0..128 {
                let b = read_disk(&mut f, ebr * 512, 512)?;
                if b[510] != 0x55 || b[511] != 0xAA {
                    break;
                }
                let (lty, lrel, ln) = entry(&b, 0);
                if lty != 0 && ln != 0 {
                    parts.push(Part { num, start: (ebr + lrel) * 512, len: ln * 512, desc: format!("lógica tipo 0x{lty:02X}") });
                    num += 1;
                }
                let (nty, nrel, _) = entry(&b, 1);
                if nty == 0 || nrel == 0 {
                    break;
                }
                ebr = lba + nrel;
            }
            continue;
        }
        if ty == 0xEE {
            continue;
        }
        parts.push(Part { num: i as u32 + 1, start: lba * 512, len: n * 512, desc: format!("tipo 0x{ty:02X}") });
    }
    Ok(parts)
}

// ------------------------------------------------------------- escaneo

pub fn scan(disks: &[Disk]) -> Result<Vec<Volume>> {
    let mut vols = Vec::new();
    for (di, d) in disks.iter().enumerate() {
        let parts = partitions(d).with_context(|| format!("leyendo particiones de {}", d.name))?;
        if parts.is_empty() {
            vols.push(Volume {
                id: d.name.clone(),
                desc: "disco completo (sin tabla de particiones)".into(),
                size: d.size,
                kind: FsKind::Unknown,
                segs: vec![Seg { start: 0, len: d.size, disk: di, disk_off: 0 }],
            });
        }
        for p in parts {
            vols.push(Volume {
                id: format!("{}:{}", d.name, p.num),
                desc: p.desc,
                size: p.len,
                kind: FsKind::Unknown,
                segs: vec![Seg { start: 0, len: p.len, disk: di, disk_off: p.start }],
            });
        }
    }
    for v in &mut vols {
        let mut r = VolReader::new(disks, &v.segs, v.size)?;
        v.kind = detect(&mut r)?;
    }

    // LVM: PVs -> metadatos -> LVs
    let mut pvs = Vec::new(); // (uuid, segmento base)
    let mut best: std::collections::BTreeMap<String, (i64, Vec<lvm::Lv>)> = Default::default();
    for v in vols.iter().filter(|v| v.kind == FsKind::LvmPv) {
        let mut r = VolReader::new(disks, &v.segs, v.size)?;
        let Some(pv) = lvm::read_pv(&mut r)? else { continue };
        pvs.push((lvm::pv_uuid_norm(&pv.uuid), v.segs[0].clone()));
        if let Some(text) = pv.metadata {
            match lvm::logical_volumes(&text) {
                Ok((vg, seq, lvs)) => {
                    if best.get(&vg).is_none_or(|(s, _)| seq > *s) {
                        best.insert(vg, (seq, lvs));
                    }
                }
                Err(e) => eprintln!("aviso: metadatos LVM ilegibles en {}: {e:#}", v.id),
            }
        }
    }
    for (_, (_, lvs)) in best {
        for lv in lvs {
            let mut segs = Vec::new();
            let mut missing = false;
            for s in &lv.segs {
                match pvs.iter().find(|(u, _)| *u == s.pv_uuid) {
                    Some((_, base)) => segs.push(Seg { start: s.lv_start, len: s.len, disk: base.disk, disk_off: base.disk_off + s.pv_offset }),
                    None => missing = true,
                }
            }
            let mut desc = "volumen lógico LVM".to_string();
            if let Some(u) = &lv.unsupported {
                desc = format!("LVM: {u}");
            } else if missing {
                desc = "LVM: falta algún PV (¿otro disco?)".into();
            }
            let mut v = Volume { id: format!("{}/{}", lv.vg, lv.name), desc, size: lv.size, kind: FsKind::Unknown, segs };
            if !v.segs.is_empty() {
                let mut r = VolReader::new(disks, &v.segs, v.size)?;
                v.kind = detect(&mut r)?;
            }
            vols.push(v);
        }
    }
    Ok(vols)
}

pub fn find<'a>(vols: &'a [Volume], id: &str) -> Result<&'a Volume> {
    if let Some(v) = vols.iter().find(|v| v.id.eq_ignore_ascii_case(id)) {
        return Ok(v);
    }
    if let Ok(n) = id.parse::<usize>()
        && n >= 1
        && n <= vols.len()
    {
        return Ok(&vols[n - 1]);
    }
    bail!("no existe el volumen '{id}' (usa el comando 'vols' para verlos)")
}
