#!/usr/bin/env python3
"""Regenerate src/lib/monaco-entry.ts from monaco-editor's editor.main.js, minus bundled languages.

Run after upgrading monaco-editor:  python3 scripts/gen-monaco-entry.py
Why: `import "monaco-editor"` registers ~80 languages; we only ship pgsql/sql/redis/markdown/json.
"""
import pathlib, posixpath, re

root = pathlib.Path(__file__).resolve().parent.parent
src = (root / "node_modules/monaco-editor/esm/vs/editor/editor.main.js").read_text()
lines = [
    "// GENERATED from monaco-editor/esm/vs/editor/editor.main.js minus the ~80 bundled languages.",
    "// Regenerate after a monaco upgrade: python3 scripts/gen-monaco-entry.py",
    "/* eslint-disable */",
]
for imp in re.findall(r"^import '([^']+)';", src, re.M):
    if "languages/" in imp or imp.endswith(".css"):
        continue  # CSS is imported by the modules themselves; package exports map only .js
    p = posixpath.normpath(posixpath.join("editor", imp))
    lines.append(f"import 'monaco-editor/{p.removesuffix('.js')}';")
lines += [
    "import 'monaco-editor/languages/definitions/pgsql/register';",
    "import 'monaco-editor/languages/definitions/sql/register';",
    "import 'monaco-editor/languages/definitions/redis/register';",
    "import 'monaco-editor/languages/definitions/markdown/register';",
    "import 'monaco-editor/languages/features/json/register';",
    "export * from 'monaco-editor/editor/editor.api';",
]
(root / "src/lib/monaco-entry.ts").write_text("\n".join(lines) + "\n")
print(f"wrote {len(lines)} lines")
