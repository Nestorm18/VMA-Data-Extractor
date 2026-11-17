// Iconos de línea (estilo Tabler) dibujados en SVG, sin dependencias externas.
const ICONS = {
  "arrow-left": '<path d="M5 12h14M5 12l6 6M5 12l6-6"/>',
  "package": '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M12 12l8-4.5M12 12v9M12 12L4 7.5"/>',
  "upload": '<path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2"/><path d="M7 9l5-5 5 5M12 4v12"/>',
  "download": '<path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2"/><path d="M7 11l5 5 5-5M12 4v12"/>',
  "folder": '<path d="M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2"/>',
  "file": '<path d="M14 3v4a1 1 0 0 0 1 1h4"/><path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2"/>',
  "link": '<path d="M9 15l6-6"/><path d="M11 6l.5-.5a5 5 0 0 1 7 7l-.5.5M13 18l-.4.5a5 5 0 0 1-7-7l.4-.5"/>',
  "windows": '<path d="M4 6l7-1v6.5H4zM13 4.7l7-1V11.5h-7zM4 13h7v6.5l-7-1zM13 13h7v7.3l-7-1z"/>',
  "linux": '<path d="M12 3c-2.5 0-4 2-4 4.5 0 2-1 3-2 4.5S4 15 5 17c.7 1.4 2 2 3 2.5M12 3c2.5 0 4 2 4 4.5 0 2 1 3 2 4.5s2 3 1 5c-.7 1.4-2 2-3 2.5"/><path d="M8 19.5c1 .8 2.5 1.5 4 1.5s3-.7 4-1.5M10 8h.01M14 8h.01M10.5 11h3"/>',
  "other": '<rect x="4" y="5" width="16" height="12" rx="2"/><path d="M8 21h8M12 17v4"/>',
  "disk": '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 15h.01M11 15h6"/>',
  "lifebuoy": '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="4"/><path d="M15 15l3.35 3.35M9 15l-3.35 3.35M5.65 5.65L9 9M18.35 5.65L15 9"/>',
  "bolt": '<path d="M13 3v7h6l-8 11v-7H5z"/>',
  "swap": '<path d="M7 7h11l-3-3M17 17H6l3 3"/>',
  "stack": '<path d="M12 4l8 4-8 4-8-4z"/><path d="M4 12l8 4 8-4M4 16l8 4 8-4"/>',
  "search": '<circle cx="10" cy="10" r="7"/><path d="M21 21l-6-6"/>',
  "chevron": '<path d="M9 6l6 6-6 6"/>',
  "alert": '<path d="M12 9v4M12 16h.01"/><path d="M10.3 3.9L2.4 17.5a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0"/>',
  "check": '<path d="M5 12l5 5L20 7"/>',
  "x": '<path d="M18 6L6 18M6 6l12 12"/>',
  "clock": '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 3"/>',
};

function icon(name, cls = "") {
  return `<span class="ico ${cls}"><svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[name] || ICONS.file}</svg></span>`;
}

function hydrateIcons(root = document) {
  root.querySelectorAll("[data-icon]").forEach((el) => {
    if (!el.dataset.done) {
      el.insertAdjacentHTML("afterbegin", icon(el.dataset.icon));
      el.dataset.done = "1";
    }
  });
}
