import { Moon, Sun } from "lucide-react";
import { useUIStore } from "../../store/useUIStore";
import { FormField, SegmentedControl } from "../ui/formKit";

export function AppearanceSection() {
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);
  return (
    <div className="space-y-4">
      <h3 className="text-[13px] font-semibold">Appearance</h3>
      <FormField label="Theme" hint="Dark is the default. The editor and result grid follow the theme.">
        <SegmentedControl
          ariaLabel="Theme"
          value={theme}
          onChange={setTheme}
          options={[
            { value: "dark", label: <><Moon className="size-3.5" /> Dark</> },
            { value: "light", label: <><Sun className="size-3.5" /> Light</> },
          ]}
        />
      </FormField>
    </div>
  );
}
