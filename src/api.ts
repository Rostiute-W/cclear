import { invoke } from "@tauri-apps/api/core";
import type { AppInfo } from "./types";

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}
