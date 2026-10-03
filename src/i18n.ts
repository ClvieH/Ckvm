import { en } from "./locales/en";
import { zhCN } from "./locales/zh-CN";

// Per-language UI strings live in src/locales/. To add a language: create
// another file there with the SAME key set, register it below, extend the
// AppLanguage union in types.ts, and add the selector option in App.tsx.
export const TEXT = {
  cn: zhCN,
  en,
} as const;

export type AppText = (typeof TEXT)[keyof typeof TEXT];
