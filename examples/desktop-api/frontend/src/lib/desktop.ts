/**
 * Thin typed wrapper around the Alex SDK (`alex.invoke`) that the demo
 * uses everywhere. Keeping every call-site behind a named function gives
 * the example three things:
 *
 *   1. Real signatures instead of `any` chains of `alex.invoke<T>`.
 *   2. A single grep target — if the host renames a method, only this
 *      file needs to change.
 *   3. Easy mocking in tests (swap the exported `desktop` for a stub).
 *
 * Every method documents *what permission* it consumes — the matching
 * entry has to be in the app's `manifest.json` or the host will reject
 * the call.
 */
import { alex } from "@alex/sdk";
import type {
  DirectoryEntry,
  FileStat,
  FileTokenGrant,
  MenuTemplate,
  SystemCapabilities,
  SystemInfo,
  WindowInfo,
} from "@alex/sdk";

// ---------- generic invoke ----------

async function call<T>(method: string, params?: unknown): Promise<T> {
  return alex.invoke<T>(method, params);
}

// ---------- system & paths ----------

export const system = {
  /** Manifest-declared capabilities. Permission: any (always allowed). */
  capabilities: () => call<SystemCapabilities>("system.capabilities"),
  /** OS / arch / runtime version / install paths. */
  info: () => call<SystemInfo>("system.info"),
  /** Open an external URL. Permission: `system.openExternal` with origin allow-list. */
  openExternal: (url: string) => call<{ opened: boolean }>("system.openExternal", { url }),
  /** Ask the host for a WebView device permission before using browser APIs. */
  requestPermission: (permission: "camera" | "microphone" | "geolocation") =>
    call<{ permission: string; granted: boolean }>("system.requestPermission", { permission }),
};

export const paths = {
  /** Per-app persistent data dir. Permission: `paths`. */
  dataDir: () => call<string>("paths.dataDir"),
  /** Per-app cache dir. Permission: `paths`. */
  cacheDir: () => call<string>("paths.cacheDir"),
  /** OS temp dir. Permission: `paths`. */
  tempDir: () => call<string>("paths.tempDir"),
};

// ---------- storage ----------

/** Per-app key/value store. Permission: `storage`. */
export const storage = {
  get: <T = unknown>(key: string) => call<T | undefined>("storage.get", { key }),
  set: (key: string, value: unknown) => call<{ stored: boolean }>("storage.set", { key, value }),
  delete: (key: string) => call<{ deleted: boolean }>("storage.delete", { key }),
  keys: () => call<{ keys: string[] }>("storage.keys"),
  clear: () => call<{ cleared: boolean }>("storage.clear"),
};

// ---------- filesystem (sandboxed to manifest allow-list) ----------

export interface FsReadTextResult {
  path: string;
  content: string;
}
export interface FsExistsResult {
  exists: boolean;
}
export interface FsWatchResult {
  subscriptionId: string;
}

/** Filesystem access. Permission: `filesystem.{read,write,watch,drop}` + path scopes. */
export const fs = {
  readText: (path: string) => call<FsReadTextResult>("filesystem.readText", { path }),
  readBinary: (path: string) =>
    call<{ encoding: "base64"; data: string }>("filesystem.readBinary", { path }),
  writeText: (path: string, content: string) =>
    call<{ written: number }>("filesystem.writeText", { path, content }),
  writeBinary: (path: string, data: string) =>
    call<{ written: boolean }>("filesystem.writeBinary", { path, data }),
  exists: (path: string) => call<FsExistsResult>("filesystem.exists", { path }),
  stat: (path: string) => call<FileStat>("filesystem.stat", { path }),
  readDir: (path: string) => call<{ entries: DirectoryEntry[] }>("filesystem.readDir", { path }),
  createDir: (path: string, recursive = true) =>
    call<{ created: string }>("filesystem.createDir", { path, recursive }),
  rename: (from: string, to: string) => call<{ renamed: string }>("filesystem.rename", { from, to }),
  copy: (from: string, to: string) => call<{ copied: number }>("filesystem.copy", { from, to }),
  remove: (path: string, recursive = false) =>
    call<{ removed: boolean }>("filesystem.remove", { path, recursive }),
  watch: (path: string) => call<FsWatchResult>("filesystem.watch", { path }),
  unwatch: (subscriptionId: string) =>
    call<{ removed: boolean }>("filesystem.unwatch", { subscriptionId }),
};

// ---------- dialogs ----------

/** Native file/directory pickers. Permission: `dialog.{open,save}`. */
export const dialog = {
  openFile: (title?: string) => call<FileTokenGrant | null>("dialog.openFile", { title }),
  openFiles: (title?: string) => call<FileTokenGrant[]>("dialog.openFiles", { title }),
  openDirectory: (title?: string) => call<FileTokenGrant | null>("dialog.openDirectory", { title }),
  saveFile: (title?: string, suggestedName?: string) =>
    call<FileTokenGrant | null>("dialog.saveFile", { title, suggestedName }),
};

// ---------- clipboard ----------

/** OS clipboard. Permission: `clipboard.{read,write}`. */
export const clipboard = {
  readText: () => call<{ text: string }>("clipboard.readText"),
  writeText: (text: string) => call<{ written: boolean }>("clipboard.writeText", { text }),
};

// ---------- notification ----------

/** Native toast. Permission: `notification.show`. */
export const notification = {
  show: (title: string, body: string) =>
    call<{ shown: boolean }>("notification.show", { title, body }),
};

// ---------- window ----------

/** Multi-window control. Permission: `window.{manage,open}`. */
export const windowApi = {
  setTitle: (title: string) => call<{ title: string }>("window.setTitle", { title }),
  list: () => call<{ windows: WindowInfo[] }>("window.list"),
  getBounds: (windowId: number) =>
    call<{ windowId: number; x: number | null; y: number | null; width: number; height: number }>(
      "window.getBounds",
      { windowId },
    ),
  create: (spec: {
    url: string;
    title?: string;
    width?: number;
    height?: number;
    x?: number;
    y?: number;
  }) => call<WindowInfo>("window.create", spec),
  destroy: (windowId: number) => call<{ destroyed: boolean }>("window.destroy", { windowId }),
  setBounds: (windowId: number, bounds: { x?: number; y?: number; width?: number; height?: number }) =>
    call<{ bounds: unknown }>("window.setBounds", { windowId, ...bounds }),
  setFullscreen: (windowId: number, fullscreen: boolean) =>
    call<{ fullscreen: boolean }>("window.setFullscreen", { windowId, fullscreen }),
  isFullscreen: (windowId: number) =>
    call<{ fullscreen: boolean }>("window.isFullscreen", { windowId }),
  minimize: (windowId: number) => call<{ minimized: boolean }>("window.minimize", { windowId }),
  maximize: (windowId: number) => call<{ maximized: boolean }>("window.maximize", { windowId }),
  close: (windowId: number) => call<{ closed: boolean }>("window.close", { windowId }),
};

// ---------- menu ----------

/** Native menu templates. Permission: `menu.manage`. */
export const menu = {
  setApplicationMenu: (template: MenuTemplate) =>
    call<{ applied: boolean }>("menu.setApplicationMenu", template),
  setContextMenu: (template: MenuTemplate) =>
    call<{ applied: boolean }>("menu.setContextMenu", template),
};

// ---------- network ----------

export interface FetchResult {
  status: number;
  url: string;
  headers: Array<{ name: string; value: string }>;
  bodyEncoding: "base64";
  body: string;
  truncated: false;
}

/** HTTPS-only host fetch. Permission: `network.fetch` + origin allow-list. */
export const net = {
  fetch: (url: string, options: { method?: string; timeoutMs?: number; maxBytes?: number } = {}) =>
    call<FetchResult>("net.fetch", { url, ...options }),
};

// ---------- MCP ----------

/** Typed MCP client, including credit-streamed interactive calls and subscriptions. */
export const mcp = alex.mcp;

// ---------- tray ----------

/** System tray icons. Permission: `menu.manage` (tray piggy-backs on the menu subsystem). */
export const tray = {
  create: (spec: { icon: string; tooltip?: string }) =>
    call<{ id: string; icon: string; tooltip: string | null }>("tray.create", spec),
  destroy: (id: string) => call<{ destroyed: boolean }>("tray.destroy", { id }),
};

// ---------- shortcuts ----------

/** Global hot-keys. Permission: `shortcut.register`. */
export const shortcuts = {
  register: (accelerator: string) =>
    call<{ registered: boolean; accelerator: string }>("shortcuts.register", { accelerator }),
  unregister: (accelerator: string) =>
    call<{ unregistered: boolean }>("shortcuts.unregister", { accelerator }),
  list: () => call<{ shortcuts: string[] }>("shortcuts.list"),
};

// ---------- bundle ----------

/** One-stop facade: `desktop.storage.set(...)`, `desktop.dialog.openFile()` etc. */
export const desktop = {
  system,
  paths,
  storage,
  fs,
  dialog,
  clipboard,
  notification,
  window: windowApi,
  menu,
  tray,
  shortcuts,
  net,
  mcp,
};

export type Desktop = typeof desktop;
