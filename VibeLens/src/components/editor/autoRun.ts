// "Open this tab and run it" requests (double-click a table in the navigator). The editor pane for
// the tab consumes the request once on mount, so the request survives the tab not existing yet.
const pending = new Set<string>();
export const requestAutoRun = (tabId: string) => void pending.add(tabId);
export const consumeAutoRun = (tabId: string): boolean => pending.delete(tabId);
