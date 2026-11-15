// Bundle Monaco locally. @monaco-editor/react defaults to loading from a CDN, which the
// app CSP blocks (and which corporate networks often block anyway). We also avoid
// `import "monaco-editor"`, which pulls in ~80 languages and OOMs small build boxes.
import { loader } from "@monaco-editor/react";
import * as monaco from "./monaco-entry";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import JsonWorker from "monaco-editor/language/json/json.worker?worker";

self.MonacoEnvironment = {
  getWorker(_id: string, label: string) {
    if (label === "json") return new JsonWorker();
    return new EditorWorker();
  },
};

loader.config({ monaco: monaco as unknown as typeof import("monaco-editor") });

export { monaco };
