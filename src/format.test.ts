import { describe, expect, it } from "vitest";
import {
  formatFileTransferBytes,
  formatQueueResumeCopy,
  formatScreenCount,
  normalizeEdgeSwitchHotkeyInput,
} from "./format";
import type { AppLanguage } from "./types";

describe("normalizeEdgeSwitchHotkeyInput", () => {
  it("trims, lowercases, and strips all whitespace", () => {
    expect(normalizeEdgeSwitchHotkeyInput("  Ctrl + Shift + K ")).toBe(
      "ctrl+shift+k",
    );
    expect(normalizeEdgeSwitchHotkeyInput("ALT\t+\nShift")).toBe("alt+shift");
  });

  it("falls back to the default hotkey for empty input", () => {
    expect(normalizeEdgeSwitchHotkeyInput("")).toBe("alt+shift+k");
    expect(normalizeEdgeSwitchHotkeyInput("   ")).toBe("alt+shift+k");
  });
});

describe("formatScreenCount", () => {
  it("pluralizes in English and keeps 屏 in Chinese", () => {
    expect(formatScreenCount(1, "en" as AppLanguage)).toBe("1 screen");
    expect(formatScreenCount(3, "en" as AppLanguage)).toBe("3 screens");
    expect(formatScreenCount(2, "cn" as AppLanguage)).toBe("2 屏");
  });
});

describe("formatFileTransferBytes", () => {
  it("uses the largest fitting unit", () => {
    expect(formatFileTransferBytes(0)).toBe("0 B");
    expect(formatFileTransferBytes(1023)).toBe("1023 B");
    expect(formatFileTransferBytes(1024)).toBe("1.0 KiB");
    expect(formatFileTransferBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatFileTransferBytes(3 * 1024 * 1024 * 1024)).toBe("3.0 GiB");
  });

  it("stays exactly on the unit boundary", () => {
    expect(formatFileTransferBytes(1024 * 1024)).toBe("1.0 MiB");
    expect(formatFileTransferBytes(1024 * 1024 - 1)).toBe("1024.0 KiB");
  });
});

describe("formatQueueResumeCopy", () => {
  it("fills count and device placeholders for both languages", () => {
    expect(
      formatQueueResumeCopy(
        "上次有 {count} 个文件未传完（目标 {device}），是否继续？",
        3,
        "Machine B",
      ),
    ).toBe("上次有 3 个文件未传完（目标 Machine B），是否继续？");
    expect(
      formatQueueResumeCopy(
        "The last session left {count} file(s) unfinished (target: {device}). Continue?",
        12,
        "A",
      ),
    ).toBe(
      "The last session left 12 file(s) unfinished (target: A). Continue?",
    );
  });
});
