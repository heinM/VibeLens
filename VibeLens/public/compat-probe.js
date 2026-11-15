// Parsed by the browser before the app bundle. If this WebKit is too old to parse the syntax the
// bundle needs (class static blocks, regex lookbehind: Safari 16.4+), this file fails to parse and
// the flag stays unset, so compat.js can show a clear message instead of a blank window.
class VgProbe {
  static {
    window.__vgCompatOK = /(?<=a)b/.test("ab") && typeof structuredClone === "function" && typeof Array.prototype.at === "function";
  }
}
void VgProbe;
