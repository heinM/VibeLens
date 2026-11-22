// "Open this tab and run it" requests (double-click a table in the navigator). The editor pane for
// the tab consumes the request once on mount, so the request survives the tab not existing yet.
//
// A request can ask for a read-only run: re-running SQL the agent ran must stay inside the
// read-only transaction, because "read-only" was a reading of the text, not a guarantee.
const pending = new Set<string>();
const readOnly = new Set<string>();
export const requestAutoRun = (tabId: string, opts: { readOnly?: boolean } = {}) => {
  pending.add(tabId);
  if (opts.readOnly) readOnly.add(tabId);
  else readOnly.delete(tabId);
};
export const consumeAutoRun = (tabId: string): boolean => pending.delete(tabId);
/** Whether the request just consumed for this tab asked for a read-only run. Clears the flag. */
export const consumeAutoRunReadOnly = (tabId: string): boolean => readOnly.delete(tabId);
