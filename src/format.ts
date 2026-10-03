// Pure formatting/normalization helpers shared by the UI. Kept free of React
// and Tauri imports so they are trivially unit-testable (format.test.ts).
import type { AppLanguage } from "./types";

export function normalizeEdgeSwitchHotkeyInput(value: string) {
  const normalized = value.trim().toLowerCase().replace(/\s+/g, "");
  return normalized.length === 0 ? "alt+shift+k" : normalized;
}

export function formatScreenCount(count: number, language: AppLanguage) {
  return language === "en"
    ? `${count} ${count === 1 ? "screen" : "screens"}`
    : `${count} 屏`;
}

export function formatFileTransferBytes(bytes: number) {
  const gib = 1024 * 1024 * 1024;
  const mib = 1024 * 1024;
  if (bytes >= gib) {
    return `${(bytes / gib).toFixed(1)} GiB`;
  }
  if (bytes >= mib) {
    return `${(bytes / mib).toFixed(1)} MiB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }
  return `${bytes} B`;
}

/// Fill the {count}/{device} placeholders of the queue-resume banner copy.
export function formatQueueResumeCopy(
  template: string,
  count: number,
  device: string,
) {
  return template.replace("{count}", String(count)).replace("{device}", device);
}
