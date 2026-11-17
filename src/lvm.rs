//! Lectura mínima de LVM2 (solo lectura): etiquetas de PV, metadatos de texto y
//! volúmenes lógicos lineales/striped de una sola franja.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

use crate::volumes::VolReader;

pub struct PvInfo {
    pub uuid: String,
    /// Texto de metadatos de la VG (si este PV tiene copia).
    pub metadata: Option<String>,
}

fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn read_pv(r: &mut VolReader) -> Result<Option<PvInfo>> {
    let head = r.read_at(0, 4 * 512)?;
    let Some(sector) = (0..4).find(|s| &head[s * 512..s * 512 + 8] == b"LABELONE") else {
        return Ok(None);
    };
    let lab = &head[sector * 512..sector * 512 + 512];
    if &lab[24..32] != b"LVM2 001" {
        return Ok(None);
    }
    let pvh = sector * 512 + le_u32(lab, 20) as usize;
    let pvh_buf = r.read_at(pvh as u64, 512)?;
    let uuid = String::from_utf8_lossy(&pvh_buf[..32]).into_owned();
    // data areas (lista terminada en cero) y después metadata areas
    let mut o = 40;
    loop {
        if o + 16 > pvh_buf.len() {
            return Ok(Some(PvInfo { uuid, metadata: None }));
        }
        let (off, size) = (le_u64(&pvh_buf, o), le_u64(&pvh_buf, o + 8));
        o += 16;
        if off == 0 && size == 0 {
            break;
        }
    }
    let mut metadata = None;
    while o + 16 <= pvh_buf.len() {
        let (mda_off, mda_size) = (le_u64(&pvh_buf, o), le_u64(&pvh_buf, o + 8));
        o += 16;
        if mda_off == 0 && mda_size == 0 {
            break;
        }
        if let Ok(Some(text)) = read_mda(r, mda_off, mda_size) {
            metadata = Some(text);
            break;
        }
    }
    Ok(Some(PvInfo { uuid, metadata }))
}

fn read_mda(r: &mut VolReader, mda_off: u64, mda_size: u64) -> Result<Option<String>> {
    let h = r.read_at(mda_off, 512)?;
    if &h[4..20] != b" LVM2 x[5A%r0N*>" {
        return Ok(None);
    }
    let (rl_off, rl_size) = (le_u64(&h, 40), le_u64(&h, 48));
    if rl_size == 0 || rl_size > 64 * 1024 * 1024 {
        return Ok(None);
    }
    let text = if rl_off + rl_size > mda_size {
        // buffer circular: la parte final continúa justo después de la cabecera
        let first = mda_size - rl_off;
        let mut t = r.read_at(mda_off + rl_off, first as usize)?;
        t.extend(r.read_at(mda_off + 512, (rl_size - first) as usize)?);
        t
    } else {
        r.read_at(mda_off + rl_off, rl_size as usize)?
    };
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    Ok(Some(String::from_utf8_lossy(&text[..end]).into_owned()))
}

// ------------------------------------------------- parser del texto de metadatos

#[derive(Debug, Clone)]
pub enum Val {
    Str(String),
    Int(i64),
    Arr(Vec<Val>),
    Sec(Vec<(String, Val)>),
}

impl Val {
    pub fn get(&self, key: &str) -> Option<&Val> {
        match self {
            Val::Sec(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    fn sections(&self) -> Vec<(&str, &Val)> {
        match self {
            Val::Sec(items) => items.iter().filter(|(_, v)| matches!(v, Val::Sec(_))).map(|(k, v)| (k.as_str(), v)).collect(),
            _ => vec![],
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b' ' | b'\t' | b'\r' | b'\n' => self.i += 1,
                b'#' => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }
    fn ident(&mut self) -> Result<String> {
        self.skip();
        let st = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || b"_-.+".contains(&self.s[self.i])) {
            self.i += 1;
        }
        if st == self.i {
            bail!("metadatos LVM: se esperaba un nombre en {}", self.i);
        }
        Ok(String::from_utf8_lossy(&self.s[st..self.i]).into_owned())
    }
    fn value(&mut self) -> Result<Val> {
        match self.peek() {
            Some(b'"') => {
                self.i += 1;
                let mut out = Vec::new();
                while self.i < self.s.len() && self.s[self.i] != b'"' {
                    if self.s[self.i] == b'\\' && self.i + 1 < self.s.len() {
                        self.i += 1;
                    }
                    out.push(self.s[self.i]);
                    self.i += 1;
                }
                self.i += 1;
                Ok(Val::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    match self.peek() {
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => self.i += 1,
                        None => bail!("metadatos LVM: array sin cerrar"),
                        _ => v.push(self.value()?),
                    }
                }
                Ok(Val::Arr(v))
            }
            _ => {
                let t = self.ident()?;
                Ok(t.parse::<i64>().map(Val::Int).unwrap_or(Val::Str(t)))
            }
        }
    }
    fn body(&mut self, top: bool) -> Result<Vec<(String, Val)>> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None if top => return Ok(items),
                None => bail!("metadatos LVM: sección sin cerrar"),
                Some(b'}') if !top => {
                    self.i += 1;
                    return Ok(items);
                }
                _ => {}
            }
            let key = self.ident()?;
            match self.peek() {
                Some(b'{') => {
                    self.i += 1;
                    items.push((key, Val::Sec(self.body(false)?)));
                }
                Some(b'=') => {
                    self.i += 1;
                    items.push((key, self.value()?));
                }
                _ => bail!("metadatos LVM: sintaxis inesperada tras '{key}'"),
            }
        }
    }
}

pub fn parse(text: &str) -> Result<Val> {
    Ok(Val::Sec(Parser { s: text.as_bytes(), i: 0 }.body(true)?))
}

// --------------------------------------------------------- volúmenes lógicos

pub struct LvSeg {
    pub lv_start: u64,
    pub len: u64,
    pub pv_uuid: String,
    pub pv_offset: u64,
}

pub struct Lv {
    pub vg: String,
    pub name: String,
    pub size: u64,
    pub segs: Vec<LvSeg>,
    pub unsupported: Option<String>,
}

fn norm_uuid(s: &str) -> String {
    s.chars().filter(|c| *c != '-').collect()
}

/// Devuelve (nombre VG, seqno, LVs) a partir del texto de metadatos.
pub fn logical_volumes(text: &str) -> Result<(String, i64, Vec<Lv>)> {
    let root = parse(text)?;
    let (vg_name, vg) = root
        .sections()
        .into_iter()
        .find(|(_, v)| v.get("physical_volumes").is_some())
        .context("metadatos LVM sin VG")?;
    let seqno = vg.int("seqno").unwrap_or(0);
    let extent = vg.int("extent_size").context("extent_size")? as u64 * 512;

    let mut pvs: BTreeMap<&str, (String, u64)> = BTreeMap::new();
    for (name, pv) in vg.get("physical_volumes").unwrap().sections() {
        let id = norm_uuid(pv.str("id").unwrap_or(""));
        let pe_start = pv.int("pe_start").unwrap_or(0) as u64 * 512;
        pvs.insert(name, (id, pe_start));
    }

    let mut lvs = Vec::new();
    let Some(lv_sec) = vg.get("logical_volumes") else {
        return Ok((vg_name.to_string(), seqno, lvs));
    };
    for (lv_name, lv) in lv_sec.sections() {
        let visible = matches!(lv.get("status"), Some(Val::Arr(a)) if a.iter().any(|v| matches!(v, Val::Str(s) if s == "VISIBLE")));
        if !visible {
            continue;
        }
        let mut segs = Vec::new();
        let mut unsupported = None;
        let mut size = 0;
        for (_, seg) in lv.sections() {
            let start = seg.int("start_extent").unwrap_or(0) as u64 * extent;
            let len = seg.int("extent_count").unwrap_or(0) as u64 * extent;
            size = size.max(start + len);
            let ty = seg.str("type").unwrap_or("?");
            let stripes = seg.int("stripe_count").unwrap_or(1);
            if ty != "striped" || stripes != 1 {
                unsupported = Some(format!("tipo '{ty}' ({stripes} franjas) no soportado"));
                continue;
            }
            if let Some(Val::Arr(a)) = seg.get("stripes")
                && let [Val::Str(pv), Val::Int(pe)] = a.as_slice()
                && let Some((id, pe_start)) = pvs.get(pv.as_str())
            {
                segs.push(LvSeg { lv_start: start, len, pv_uuid: id.clone(), pv_offset: pe_start + *pe as u64 * extent });
            }
        }
        lvs.push(Lv { vg: vg_name.to_string(), name: lv_name.to_string(), size, segs, unsupported });
    }
    Ok((vg_name.to_string(), seqno, lvs))
}

pub fn pv_uuid_norm(s: &str) -> String {
    norm_uuid(s)
}
