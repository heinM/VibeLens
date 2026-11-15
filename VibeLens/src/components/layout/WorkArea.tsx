import { Group, Panel, Separator, useDefaultLayout } from "react-resizable-panels";
import { EditorArea } from "../editor/EditorArea";
import { KeyInspector } from "../valkey/KeyInspector";
import { useInspectorStore } from "../valkey/inspectorStore";
import { layoutStorage, panelStyle } from "./storage";

/** Editor tabs and results, with the Valkey key inspector docked on the right while a key is open. */
export function WorkArea() {
  const target = useInspectorStore((s) => s.target);
  const ids = target ? ["editor", "inspector"] : ["editor"];
  const layout = useDefaultLayout({ id: "vg-work", panelIds: ids, storage: layoutStorage });
  return (
    <Group orientation="horizontal" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged} className="h-full">
      <Panel style={panelStyle} id="editor" minSize={320}>
        <EditorArea />
      </Panel>
      {target && (
        <>
          <Separator className="vg-sep" />
          <Panel style={panelStyle} id="inspector" defaultSize="38%" minSize={280} maxSize="70%" groupResizeBehavior="preserve-pixel-size">
            <KeyInspector key={`${target.profileId}/${target.db}/${target.key}`} target={target} />
          </Panel>
        </>
      )}
    </Group>
  );
}
