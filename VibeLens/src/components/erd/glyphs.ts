// Icon path data (lucide "key-round" and "link-2", 24x24 viewBox, stroke 2), shared by the
// on-screen diagram and the SVG export so both draw the same marks.
export const KEY_PATHS = [
  "M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z",
  "M16.5 7.5h.01",
] as const;
export const LINK_PATHS = ["M9 17H7A5 5 0 0 1 7 7h2", "M15 7h2a5 5 0 1 1 0 10h-2", "M8 12h8"] as const;
export const TABLE_PATHS = ["M12 3v18", "M3 9h18", "M3 15h18", "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"] as const;
export const CHEVRON_DOWN = "m6 9 6 6 6-6";
export const CHEVRON_RIGHT = "m9 18 6-6-6-6";

/** Hue (degrees) for a cluster; lone tables return null and use the neutral palette. */
export function clusterHue(cluster: number): number | null {
  if (cluster < 0) return null;
  // Golden-angle steps keep neighbouring clusters visibly apart, starting from the app's teal.
  return Math.round((172 + cluster * 137.508) % 360);
}
