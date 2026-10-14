// SPDX-License-Identifier: AGPL-3.0-or-later
// The global Tauri injects when withGlobalTauri is on; only the part the UI uses is declared.
interface TauriCore {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
}

interface Window {
  __TAURI__: { core: TauriCore };
}
