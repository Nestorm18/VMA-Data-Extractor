# vmatool — VMA Data Extractor

Extrae y explora backups de Proxmox (`vzdump-qemu-*.vma`, `.vma.zst`, `.vma.gz`) **sin restaurar la VM**. Funciona en Windows y en Linux, y lee discos de VMs Linux (ext2/3/4, también dentro de LVM) y Windows (NTFS).

## Aplicación con interfaz (VMA Data Extractor)

`VMA Data Extractor.exe` es un único ejecutable portable (Windows 10/11, usa WebView2 que ya viene con Windows).

1. Arrastra un backup a la ventana o pulsa *Abrir backup*.
2. Comprueba el destino: avisa antes de extraer si no hay espacio (usa el `.log` de vzdump si está junto al backup; si no, analiza el backup primero, lo que además verifica el checksum).
3. *Extraer* muestra una barra de progreso con velocidad y tiempo restante.
4. Al terminar se abre el explorador: elige el volumen (Windows (C:), Linux (/), /home...), marca archivos o carpetas y pulsa *Copiar a…*.
5. Al cerrar pregunta si quieres borrar los archivos extraídos.

### Datos que guarda la aplicación

Solo guarda la lista de **recientes** (los últimos 8 backups abiertos: ruta, nombre de la VM, fecha y tamaño), en:

| Sistema | Archivo |
|---|---|
| Windows | `%APPDATA%\com.vmatool.extractor\recientes.json` |
| Linux | `~/.local/share/com.vmatool.extractor/recientes.json` |

Para vaciar la lista basta con borrar ese archivo. Los backups que ya no existen no se muestran.

Cada extracción deja en su carpeta de destino:

- `disk-<disco>.raw`: un archivo por disco de la VM (disperso).
- `qemu-server.conf`: la configuración de la VM.
- `extract-report.txt`: resultado y errores de la extracción.

Al cerrar, *Borrar* elimina solo esos archivos (y la carpeta si queda vacía).

Compilar la interfaz (no necesita Node):

```
cargo build --release -p vma-extractor
```

Para probar la interfaz en un navegador con datos simulados: `python -m http.server 8765 --directory gui` y abrir `http://localhost:8765/dev/`.

## Compilar la línea de comandos

```
cargo build --release
```

El ejecutable queda en `target\release\vmatool.exe`.

## Uso

```
vmatool info    <backup>                     # configuración y discos (instantáneo)
vmatool verify  <backup>                     # lee todo y comprueba el checksum
vmatool extract <backup> <carpeta>           # discos -> .raw dispersos
vmatool extract <backup> <carpeta> --ignore-checksum   # backup dañado: saca todo lo posible
vmatool vols    <carpeta>                    # particiones, LVM y sistemas de archivos
vmatool ls      <carpeta> <vol> [ruta]       # listar carpeta
vmatool get     <carpeta> <vol> <ruta> [destino]  # copiar archivo o carpeta al PC
vmatool shell   <carpeta> [vol]              # explorador interactivo: ls, cd, get, use...
```

`<vol>` es el id que muestra `vols` (`scsi0:1`, `scsi1`, `vg/root`...) o su número.

Ejemplo:

```
vmatool extract vzdump-qemu-5000-2025_11_03-19_21_05.vma.zst extraido5000
vmatool vols extraido5000
vmatool shell extraido5000
scsi0:1:/> cd /home
scsi0:1:/home> get usuario C:\Recuperado
```

## Cómo funciona

1. Descomprime el flujo (zstd/gzip) sin cargarlo en memoria.
2. Escribe cada disco como `.raw` **disperso**: los bloques vacíos no ocupan espacio, así que un disco de 50 GiB con 3 GiB de datos ocupa unos 3 GiB.
3. Lee la tabla de particiones (MBR/GPT), los volúmenes LVM y el sistema de archivos directamente del `.raw`, en solo lectura.

### Backups dañados

- `--ignore-checksum` desactiva la comprobación de zstd, que es la que hace fallar a `qmrestore` con `Restored data doesn't match checksum`.
- Si una cabecera de extent está dañada, busca la siguiente y sigue; si el archivo se corta, guarda todo lo leído hasta ese punto.
- Al copiar, un archivo ilegible no detiene la copia: se anota el error y se continúa.
- Cada extracción deja un `extract-report.txt` con el resultado.

## Estructura del proyecto

```
src/            núcleo (librería) + CLI vmatool
  vma.rs        lectura del formato VMA, extracción, análisis, .log de vzdump
  volumes.rs    discos .raw, particiones MBR/GPT, detección de sistemas de archivos
  lvm.rs        volúmenes LVM2
  fs.rs         navegación y copia en ext2/3/4 y NTFS
  sparse.rs     archivos dispersos, espacio libre
  main.rs       línea de comandos
gui/            aplicación Tauri
  src/main.rs   comandos que usa la interfaz
  ui/           interfaz (HTML, CSS y JS sin dependencias)
  dev/          simulación para probar la interfaz en un navegador
  tauri.conf.json, icons/, capabilities/
```

## Limitaciones

- XFS, Btrfs, LUKS, BitLocker y LVM thin/RAID se detectan pero no se pueden explorar (los `.raw` sí se extraen y se pueden montar en Linux).
- Archivos NTFS comprimidos o cifrados (EFS) no se pueden copiar.
- Los enlaces simbólicos y puntos de unión se omiten al copiar.
- `.vma.lzo`: descomprímelo antes con `lzop -d`.
