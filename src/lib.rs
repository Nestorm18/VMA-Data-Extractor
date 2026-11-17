//! Núcleo de vmatool: lectura de backups VMA de Proxmox, discos extraídos y
//! sistemas de archivos (ext4/NTFS). Lo usan la CLI y la interfaz gráfica.

pub mod fs;
pub mod lvm;
pub mod sparse;
pub mod vma;
pub mod volumes;

/// Tamaño legible: 1.50 GiB, 820.00 MiB...
pub fn human(n: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.2} {}", U[i]) }
}
