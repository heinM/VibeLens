export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
/** Display name of the primary modifier key. */
export const MOD = isMac ? "⌘" : "Ctrl";
