import { useState } from "react";
import { Info, LayoutPanelLeft, Palette, Sparkles } from "lucide-react";
import { cn } from "../../lib/cn";
import { useUIStore } from "../../store/useUIStore";
import { Dialog } from "../ui";
import { FocusScope } from "../ui/formKit";
import { AboutSection } from "./AboutSection";
import { AppearanceSection } from "./AppearanceSection";
import { InterfaceSection } from "./InterfaceSection";
import { ProvidersSection } from "./ProvidersSection";

type Tab = "interface" | "providers" | "appearance" | "about";

const NAV: { id: Tab; label: string; icon: typeof Sparkles }[] = [
  { id: "interface", label: "Interface & AI", icon: LayoutPanelLeft },
  { id: "providers", label: "AI providers", icon: Sparkles },
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "about", label: "About", icon: Info },
];

/** Mounted once; renders nothing until `useUIStore().settingsOpen`. */
export function SettingsDialog() {
  const open = useUIStore((s) => s.settingsOpen);
  const setOpen = useUIStore((s) => s.setSettingsOpen);
  const aiMode = useUIStore((s) => s.aiMode);
  const [tab, setTab] = useState<Tab>("interface");
  // In classic mode the AI provider settings are hidden until the user turns AI on.
  const nav = NAV.filter((n) => !(n.id === "providers" && aiMode === "classic"));
  const current: Tab = nav.some((n) => n.id === tab) ? tab : "interface";
  if (!open) return null;
  const close = () => {
    // A nested confirmation (e.g. delete provider) owns Esc while it is open.
    if (document.querySelectorAll('[role="dialog"]').length > 1) return;
    setOpen(false);
  };
  return (
    <Dialog open onClose={close} title="Settings" width="max-w-3xl">
      <FocusScope>
        <div className="-m-4 flex h-[min(600px,74vh)] min-h-[360px]">
          <nav aria-label="Settings sections" className="flex w-44 shrink-0 flex-col gap-0.5 border-r border-border bg-bg/40 p-2">
            {nav.map(({ id, label, icon: Icon }) => (
              <button
                key={id}
                type="button"
                data-autofocus={id === current ? "" : undefined}
                aria-current={current === id ? "page" : undefined}
                onClick={() => setTab(id)}
                className={cn(
                  "flex h-8 items-center gap-2 rounded-md px-2 text-left text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
                  current === id ? "bg-panel-2 text-fg" : "text-muted hover:bg-panel-2/60 hover:text-fg",
                )}
              >
                <Icon className={cn("size-3.5", current === id && "text-accent")} />
                {label}
              </button>
            ))}
          </nav>
          <div className="min-w-0 flex-1 overflow-y-auto p-5">
            <div className="mx-auto max-w-2xl">
              {current === "interface" && <InterfaceSection onConfigureProviders={() => setTab("providers")} />}
              {current === "providers" && <ProvidersSection />}
              {current === "appearance" && <AppearanceSection />}
              {current === "about" && <AboutSection />}
            </div>
          </div>
        </div>
      </FocusScope>
    </Dialog>
  );
}
