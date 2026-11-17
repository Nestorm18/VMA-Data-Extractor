//! Archivos dispersos: los bloques vacíos del disco no ocupan espacio real.

use std::fs::File;

/// Devuelve false si el sistema de archivos no admite archivos dispersos.
#[cfg(windows)]
pub fn make_sparse(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let mut returned = 0u32;
    // Si falla (p. ej. FAT32 o recurso de red sin soporte), el archivo será normal.
    unsafe {
        0 != DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(not(windows))]
pub fn make_sparse(_file: &File) -> bool {
    // En Linux/macOS los huecos creados con set_len + seek ya son dispersos.
    true
}

/// Primera carpeta existente de `path` (o el propio path).
fn existing_ancestor(path: &std::path::Path) -> std::path::PathBuf {
    let mut p = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    while !p.exists() {
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => break,
        }
    }
    p
}

/// Espacio libre (bytes) en el volumen donde está `path`, y si admite archivos dispersos.
#[cfg(windows)]
pub fn volume_info(path: &std::path::Path) -> Option<(u64, bool)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW, GetVolumePathNameW};
    let p = existing_ancestor(path);
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 {
        return None;
    }
    let mut root = [0u16; 1024];
    let mut flags = 0u32;
    let sparse = unsafe {
        GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
            && GetVolumeInformationW(root.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut flags, std::ptr::null_mut(), 0) != 0
    } && flags & 0x40 != 0; // FILE_SUPPORTS_SPARSE_FILES
    Some((free, sparse))
}

#[cfg(unix)]
pub fn volume_info(path: &std::path::Path) -> Option<(u64, bool)> {
    use std::os::unix::ffi::OsStrExt;
    let p = existing_ancestor(path);
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64 * st.f_frsize as u64, true))
}

/// Espacio que ocupa realmente un archivo en disco (descontando huecos dispersos).
#[cfg(windows)]
pub fn allocated_size(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0u32;
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == u32::MAX && std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0 {
        return None;
    }
    Some(((high as u64) << 32) | low as u64)
}

#[cfg(unix)]
pub fn allocated_size(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.blocks() * 512)
}
