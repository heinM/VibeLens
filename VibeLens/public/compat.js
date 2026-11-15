// Shows a readable "WebKit too old" screen when compat-probe.js could not run (see that file).
(function () {
  if (window.__vgCompatOK === true) return;
  window.__vgUnsupported = true;
  var root = document.getElementById("root");
  if (!root) return;
  var ua = navigator.userAgent || "";
  var m = /Version\/([\d.]+)/.exec(ua);
  root.innerHTML =
    '<div style="font:14px -apple-system,system-ui,sans-serif;color:#e6e6e6;background:#0f1115;height:100%;display:flex;align-items:center;justify-content:center;padding:32px">' +
    '<div style="max-width:560px;border:1px solid #2a2e37;border-radius:12px;padding:24px;background:#161920">' +
    '<h1 style="font-size:16px;margin:0 0 8px">VibeLens needs a newer system WebKit</h1>' +
    '<p style="margin:0 0 8px;color:#9aa3b2">VibeLens renders with the operating system’s web engine. It needs the engine from Safari 16.4 or later' +
    (m ? " (this Mac has Safari " + m[1].replace(/[^\d.]/g, "") + ")" : "") +
    ".</p>" +
    '<p style="margin:0;color:#9aa3b2">On macOS, update Safari via System Settings → General → Software Update (Safari 16.4+ is available for macOS 11 Big Sur and later). On Linux, update WebKitGTK to 2.40 or later.</p>' +
    "</div></div>";
})();
