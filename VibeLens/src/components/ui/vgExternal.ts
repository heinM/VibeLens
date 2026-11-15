import { openUrl } from "@tauri-apps/plugin-opener";

/** Open an https URL in the system browser. Other schemes are ignored. */
export async function openExternal(url: string): Promise<void> {
  if (!/^https:\/\//i.test(url)) return; // capability only allows https
  try {
    await openUrl(url);
  } catch {
    // Outside Tauri (tests, plain vite dev) fall back to the browser.
    window.open(url, "_blank", "noopener,noreferrer");
  }
}
