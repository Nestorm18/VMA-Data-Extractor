//! Lector del formato VMA de Proxmox (vzdump), comprimido o no.
//!
//! Formato (todo big-endian salvo el tamaño de los blobs):
//!   cabecera (header_size bytes): magic "VMA\0", uuid, configs, lista de discos
//!   extents: cabecera de 512 bytes ("VMAE") + datos de los bloques de 4 KiB no vacíos

use anyhow::{Context, Result, bail};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const CLUSTER: u64 = 64 * 1024;
pub const BLOCK: u64 = 4 * 1024;
const EXTENT_HEADER: usize = 512;
const BLOCKS_PER_EXTENT: usize = 59;

pub struct Device {
    pub id: u8,
    pub name: String,
    pub size: u64,
}

pub struct Header {
    pub version: u32,
    pub uuid: [u8; 16],
    pub ctime: i64,
    pub configs: Vec<(String, Vec<u8>)>,
    pub devices: Vec<Device>,
}

impl Header {
    pub fn device(&self, id: u8) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }
}

/// Cuenta los bytes leídos del archivo comprimido (para el progreso).
struct Counting<R> {
    inner: R,
    count: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

/// Flujo descomprimido con posibilidad de "devolver" bytes (para resincronizar).
pub struct Stream {
    inner: Box<dyn Read>,
    pending: Vec<u8>,
    /// Posición en el flujo descomprimido.
    pub pos: u64,
    /// Bytes leídos del archivo de entrada.
    pub input_read: Rc<Cell<u64>>,
    pub input_size: u64,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = if !self.pending.is_empty() {
            let n = buf.len().min(self.pending.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            n
        } else {
            self.inner.read(buf)?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Stream {
    fn unread(&mut self, data: &[u8]) {
        let mut v = data.to_vec();
        v.extend_from_slice(&self.pending);
        self.pending = v;
        self.pos -= data.len() as u64;
    }

    /// Lee hasta `n` bytes; devuelve menos solo si se acaba el flujo.
    fn read_up_to(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match self.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    buf.truncate(got);
                    return Err(io::Error::new(e.kind(), PartialRead(buf, e.to_string())));
                }
            }
        }
        buf.truncate(got);
        Ok(buf)
    }
}

/// Error de lectura que conserva los bytes que sí se leyeron antes del fallo.
#[derive(Debug)]
struct PartialRead(Vec<u8>, String);
impl std::fmt::Display for PartialRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}
impl std::error::Error for PartialRead {}

fn split_partial(e: io::Error) -> (Vec<u8>, String) {
    let msg = e.to_string();
    match e.into_inner() {
        Some(inner) => match inner.downcast::<PartialRead>() {
            Ok(p) => (p.0, p.1),
            Err(_) => (Vec::new(), msg),
        },
        None => (Vec::new(), msg),
    }
}

/// Abre un backup .vma, .vma.zst o .vma.gz.
pub fn open(path: &Path, ignore_checksum: bool) -> Result<Stream> {
    let mut f = File::open(path).with_context(|| format!("no se puede abrir {}", path.display()))?;
    let input_size = f.metadata()?.len();
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).context("archivo demasiado corto")?;
    f.seek(SeekFrom::Start(0))?;
    let count = Rc::new(Cell::new(0));
    let counting = Counting { inner: f, count: count.clone() };
    let buffered = BufReader::with_capacity(1 << 20, counting);

    let inner: Box<dyn Read> = match magic {
        [0x28, 0xB5, 0x2F, 0xFD] => {
            let mut dec = zstd::stream::raw::Decoder::new()?;
            if ignore_checksum {
                dec.set_parameter(zstd::zstd_safe::DParameter::ForceIgnoreChecksum(true))?;
            }
            Box::new(zstd::stream::zio::Reader::new(buffered, dec))
        }
        [0x1F, 0x8B, _, _] => Box::new(flate2::bufread::MultiGzDecoder::new(buffered)),
        [b'V', b'M', b'A', 0] => Box::new(buffered),
        [0x89, b'L', b'Z', b'O'] => bail!("formato .lzo no soportado: descomprímelo antes con 'lzop -d'"),
        _ => bail!("no parece un backup VMA de Proxmox (.vma / .vma.zst / .vma.gz)"),
    };
    Ok(Stream { inner, pending: Vec::new(), pos: 0, input_read: count, input_size })
}

fn be_u16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes(b[o..o + 2].try_into().unwrap())
}
fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be_u64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn read_header(s: &mut Stream) -> Result<Header> {
    let fixed = s.read_up_to(12288).context("error leyendo la cabecera VMA")?;
    if fixed.len() < 12288 || &fixed[..4] != b"VMA\0" {
        bail!("cabecera VMA no válida");
    }
    let version = be_u32(&fixed, 4);
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&fixed[8..24]);
    let ctime = be_u64(&fixed, 24) as i64;
    let blob_off = be_u32(&fixed, 48) as usize;
    let blob_size = be_u32(&fixed, 52) as usize;
    let header_size = be_u32(&fixed, 56) as usize;
    if header_size < 12288 || header_size > 64 * 1024 * 1024 {
        bail!("tamaño de cabecera VMA no válido: {header_size}");
    }
    let mut head = fixed;
    head.extend(s.read_up_to(header_size - 12288)?);
    if head.len() < header_size || blob_off + blob_size > header_size {
        bail!("cabecera VMA truncada");
    }

    let blob = |ptr: u32| -> Result<&[u8]> {
        let pos = blob_off + ptr as usize;
        if pos + 2 > head.len() {
            bail!("puntero de blob fuera de rango");
        }
        let size = head[pos] as usize | (head[pos + 1] as usize) << 8;
        head.get(pos + 2..pos + 2 + size).context("blob fuera de rango")
    };
    let cstr = |b: &[u8]| String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or(b)).into_owned();

    let mut configs = Vec::new();
    for i in 0..256 {
        let name_ptr = be_u32(&head, 2044 + i * 4);
        let data_ptr = be_u32(&head, 3068 + i * 4);
        if name_ptr == 0 {
            continue;
        }
        configs.push((cstr(blob(name_ptr)?), blob(data_ptr)?.to_vec()));
    }

    let mut devices = Vec::new();
    for i in 1..256usize {
        let off = 4096 + i * 32;
        let ptr = be_u32(&head, off);
        if ptr == 0 {
            continue;
        }
        devices.push(Device { id: i as u8, name: cstr(blob(ptr)?), size: be_u64(&head, off + 8) });
    }
    Ok(Header { version, uuid, ctime, configs, devices })
}

#[derive(Default)]
pub struct DevStats {
    pub data_blocks: u64,
    pub zero_blocks: u64,
}

#[derive(Default)]
pub struct WalkResult {
    pub extents: u64,
    pub complete: bool,
    pub cancelled: bool,
    pub errors: Vec<String>,
    pub skipped_bytes: u64,
    pub stats: std::collections::BTreeMap<u8, DevStats>,
}

fn extent_ok(eh: &[u8], uuid: &[u8; 16]) -> Option<usize> {
    if eh.len() < EXTENT_HEADER || &eh[..4] != b"VMAE" || &eh[8..24] != uuid {
        return None;
    }
    let block_count = be_u16(eh, 6) as usize;
    let mut needed = 0usize;
    for k in 0..BLOCKS_PER_EXTENT {
        let bi = be_u64(eh, 40 + k * 8);
        if bi == 0 {
            continue;
        }
        let mask = (bi >> 48) as u16;
        needed += mask.count_ones() as usize;
    }
    (needed == block_count).then_some(block_count)
}

/// Busca la siguiente cabecera de extent válida. Devuelve false si se acaba el flujo.
fn resync(s: &mut Stream, uuid: &[u8; 16], skipped: &mut u64) -> Result<bool, String> {
    let mut window: Vec<u8> = Vec::new();
    loop {
        let chunk = s.read_up_to(1 << 20).map_err(|e| split_partial(e).1)?;
        let eof = chunk.is_empty();
        window.extend_from_slice(&chunk);
        let mut i = 0;
        while i + EXTENT_HEADER <= window.len() {
            if &window[i..i + 4] == b"VMAE" && &window[i + 8..i + 24] == uuid && extent_ok(&window[i..], uuid).is_some() {
                *skipped += i as u64;
                s.unread(&window[i..]);
                return Ok(true);
            }
            i += 1;
        }
        if eof {
            *skipped += window.len() as u64;
            return Ok(false);
        }
        // conserva la cola por si una cabecera queda partida entre dos trozos
        let keep = window.len().min(EXTENT_HEADER - 1);
        *skipped += (window.len() - keep) as u64;
        window.drain(..window.len() - keep);
    }
}

/// Recorre todos los extents. `on_data(dev, offset, datos)` recibe cada trozo de datos no vacío.
pub fn walk(
    s: &mut Stream,
    h: &Header,
    mut on_data: impl FnMut(u8, u64, &[u8]) -> io::Result<()>,
    mut progress: impl FnMut(&Stream),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<WalkResult> {
    let mut r = WalkResult::default();
    for d in &h.devices {
        r.stats.insert(d.id, DevStats::default());
    }
    let mut last = Instant::now();

    loop {
        if last.elapsed() > Duration::from_millis(250) {
            progress(s);
            last = Instant::now();
        }
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            r.cancelled = true;
            r.errors.push("cancelado por el usuario".into());
            break;
        }
        let start = s.pos;
        let eh = match s.read_up_to(EXTENT_HEADER) {
            Ok(v) => v,
            Err(e) => {
                r.errors.push(format!("error de lectura en el byte {start} del flujo: {}", split_partial(e).1));
                break;
            }
        };
        if eh.is_empty() {
            r.complete = true;
            break;
        }
        let Some(block_count) = extent_ok(&eh, &h.uuid) else {
            if eh.len() < EXTENT_HEADER {
                r.errors.push(format!("el flujo termina dentro de una cabecera de extent (byte {start})"));
                break;
            }
            r.errors.push(format!("cabecera de extent dañada en el byte {start}; buscando la siguiente..."));
            s.unread(&eh[1..]);
            r.skipped_bytes += 1;
            match resync(s, &h.uuid, &mut r.skipped_bytes) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => {
                    r.errors.push(format!("error de lectura buscando extents: {e}"));
                    break;
                }
            }
        };

        let want = block_count * BLOCK as usize;
        let (data, read_err) = match s.read_up_to(want) {
            Ok(v) => (v, None),
            Err(e) => {
                let (partial, msg) = split_partial(e);
                (partial, Some(msg))
            }
        };

        // Escribe los bloques disponibles (aunque el extent esté incompleto).
        let mut pos = 0usize;
        'blocks: for k in 0..BLOCKS_PER_EXTENT {
            let bi = be_u64(&eh, 40 + k * 8);
            if bi == 0 {
                continue;
            }
            let mask = (bi >> 48) as u16;
            let dev = ((bi >> 32) & 0xFF) as u8;
            let cluster = bi & 0xFFFF_FFFF;
            let base = cluster * CLUSTER;
            let known = h.device(dev).is_some();
            let st = r.stats.entry(dev).or_default();
            if mask == 0xFFFF {
                let end = pos + CLUSTER as usize;
                if end > data.len() {
                    break 'blocks;
                }
                if known {
                    if let Err(e) = on_data(dev, base, &data[pos..end]) {
                        r.errors.push(format!("error escribiendo el disco: {e}"));
                        progress(s);
                        return Ok(r);
                    }
                }
                st.data_blocks += 16;
                pos = end;
            } else {
                for j in 0..16u64 {
                    if mask & (1 << j) != 0 {
                        let end = pos + BLOCK as usize;
                        if end > data.len() {
                            break 'blocks;
                        }
                        if known {
                            if let Err(e) = on_data(dev, base + j * BLOCK, &data[pos..end]) {
                                r.errors.push(format!("error escribiendo el disco: {e}"));
                                progress(s);
                                return Ok(r);
                            }
                        }
                        st.data_blocks += 1;
                        pos = end;
                    } else {
                        st.zero_blocks += 1;
                    }
                }
            }
            if !known {
                r.errors.push(format!("extent con disco desconocido (id {dev}) ignorado"));
            }
        }
        r.extents += 1;

        if let Some(msg) = read_err {
            r.errors.push(format!("error de lectura en el byte {} del flujo: {msg}", s.pos));
            break;
        }
        if data.len() < want {
            r.errors.push(format!("el flujo termina a mitad de un extent (byte {})", s.pos));
            break;
        }
    }
    progress(s);
    Ok(r)
}

// ---------------------------------------------------------------- extracción

pub fn disk_file_name(dev_name: &str) -> String {
    format!("disk-{}.raw", sanitize(dev_name))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
        .collect()
}

/// Progreso de una lectura completa del backup.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Progress {
    /// 0.0 - 1.0
    pub fraction: f64,
    pub input_read: u64,
    pub input_size: u64,
    pub bytes_per_sec: u64,
    pub eta_secs: Option<u64>,
}

fn make_progress(s: &Stream, started: Instant) -> Progress {
    let done = s.input_read.get();
    let fraction = if s.input_size > 0 { (done as f64 / s.input_size as f64).min(1.0) } else { 0.0 };
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let rate = done as f64 / secs;
    let eta = (rate > 0.0 && secs > 2.0).then(|| (s.input_size.saturating_sub(done) as f64 / rate) as u64);
    Progress { fraction, input_read: done, input_size: s.input_size, bytes_per_sec: rate as u64, eta_secs: eta }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DiskReport {
    pub name: String,
    pub file: String,
    pub size: u64,
    pub data_bytes: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Report {
    pub complete: bool,
    pub cancelled: bool,
    pub extents: u64,
    pub skipped_bytes: u64,
    pub errors: Vec<String>,
    pub disks: Vec<DiskReport>,
    pub elapsed_secs: u64,
    /// El fallo parece de checksum (recuperable con ignore_checksum).
    pub checksum_error: bool,
}

impl Report {
    fn new(h: &Header, r: &WalkResult, started: Instant) -> Self {
        let disks = h
            .devices
            .iter()
            .map(|d| DiskReport {
                name: d.name.clone(),
                file: disk_file_name(&d.name),
                size: d.size,
                data_bytes: r.stats.get(&d.id).map(|s| s.data_blocks * BLOCK).unwrap_or(0),
            })
            .collect();
        Report {
            complete: r.complete && !r.cancelled,
            cancelled: r.cancelled,
            extents: r.extents,
            skipped_bytes: r.skipped_bytes,
            checksum_error: r.errors.iter().any(|e| e.to_lowercase().contains("checksum")),
            errors: r.errors.clone(),
            disks,
            elapsed_secs: started.elapsed().as_secs(),
        }
    }

    pub fn to_text(&self, input: &Path) -> String {
        let mut t = format!("origen: {}\nextents: {}\ncompleto: {}\n", input.display(), self.extents, self.complete);
        for d in &self.disks {
            t.push_str(&format!("{}: {} con datos de {}\n", d.name, crate::human(d.data_bytes), crate::human(d.size)));
        }
        if self.skipped_bytes > 0 {
            t.push_str(&format!("bytes saltados por daños: {}\n", self.skipped_bytes));
        }
        for e in &self.errors {
            t.push_str(&format!("ERROR: {e}\n"));
        }
        t
    }
}

struct OutDisk {
    file: File,
    pos: u64,
}

/// Extrae los discos a `outdir`. `progress` se llama varias veces por segundo.
pub fn extract_with(
    input: &Path,
    outdir: &Path,
    ignore_checksum: bool,
    force: bool,
    mut progress: impl FnMut(&Progress),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Report> {
    let mut s = open(input, ignore_checksum)?;
    let h = read_header(&mut s)?;
    std::fs::create_dir_all(outdir).with_context(|| format!("no se puede crear {}", outdir.display()))?;

    for (name, data) in &h.configs {
        std::fs::write(outdir.join(sanitize(name)), data)?;
    }

    let mut outs: std::collections::HashMap<u8, OutDisk> = Default::default();
    for d in &h.devices {
        let p = outdir.join(disk_file_name(&d.name));
        if p.exists() && !force {
            bail!("{} ya existe", p.display());
        }
        let file = File::create(&p).with_context(|| format!("no se puede crear {}", p.display()))?;
        if !crate::sparse::make_sparse(&file) {
            eprintln!("AVISO: el destino no admite archivos dispersos; {} ocupará {} completos", p.display(), crate::human(d.size));
        }
        file.set_len(d.size)?;
        outs.insert(d.id, OutDisk { file, pos: 0 });
    }

    let started = Instant::now();
    let r = walk(
        &mut s,
        &h,
        |dev, off, data| {
            let o = outs.get_mut(&dev).unwrap();
            if o.pos != off {
                o.file.seek(SeekFrom::Start(off))?;
            }
            o.file.write_all(data)?;
            o.pos = off + data.len() as u64;
            Ok(())
        },
        |s| progress(&make_progress(s, started)),
        cancel,
    )?;
    for o in outs.values_mut() {
        o.file.flush()?;
    }
    let report = Report::new(&h, &r, started);
    std::fs::write(outdir.join("extract-report.txt"), report.to_text(input))?;
    Ok(report)
}

/// Lee el backup entero sin escribir nada: datos reales por disco y checksum.
pub fn scan_with(input: &Path, mut progress: impl FnMut(&Progress), cancel: &std::sync::atomic::AtomicBool) -> Result<Report> {
    let mut s = open(input, false)?;
    let h = read_header(&mut s)?;
    let started = Instant::now();
    let r = walk(&mut s, &h, |_, _, _| Ok(()), |s| progress(&make_progress(s, started)), cancel)?;
    Ok(Report::new(&h, &r, started))
}

/// Datos reales según el .log de vzdump que suele acompañar al backup.
/// Devuelve (bytes con datos, bytes totales de los discos).
pub fn data_size_from_log(input: &Path) -> Option<(u64, u64)> {
    let name = input.file_name()?.to_str()?;
    let stem = name.split(".vma").next()?;
    let log = std::fs::read_to_string(input.with_file_name(format!("{stem}.log"))).ok()?;
    let parse_size = |s: &str| -> Option<u64> {
        let mut it = s.split_whitespace();
        let n: f64 = it.next()?.replace(',', ".").parse().ok()?;
        let mult = match it.next()? {
            "B" => 1.0,
            "KiB" => 1024.0,
            "MiB" => 1024f64.powi(2),
            "GiB" => 1024f64.powi(3),
            "TiB" => 1024f64.powi(4),
            _ => return None,
        };
        Some((n * mult) as u64)
    };
    let mut total = None;
    let mut zero = None;
    for line in log.lines() {
        if let Some(i) = line.find("transferred ") {
            total = parse_size(&line[i + 12..]);
        }
        if let Some(i) = line.find("backup is sparse: ") {
            zero = parse_size(&line[i + 18..]);
        }
    }
    let total = total?;
    Some((total.saturating_sub(zero.unwrap_or(0)), total))
}
