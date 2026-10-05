import { Dialog, DialogTrigger, Popover } from "react-aria-components";
import { ModelPicker } from "./ModelPicker";
import { Button } from "./ui";
import { ShellIcon } from "./ShellIcon";
import type { Snapshot } from "./bridge";

/** The composer model chip opens the same native-backed picker as Settings. */
export function ModelMenu({ snapshot, busy, onSelect }: { snapshot: Snapshot; busy: boolean; onSelect: (provider: string, model: string, variant: string | null) => Promise<void> }) {
  return <DialogTrigger>
    <Button className="composer-model ghost" aria-label="Model settings" title="Change provider, model and reasoning">
      <span className="model-label" title={`${snapshot.provider} / ${snapshot.model} · Reasoning: ${snapshot.variant || "Default"}`}><span className="model-name">{snapshot.provider} / {snapshot.model}</span><span className="model-variant" aria-label={`Reasoning variant: ${snapshot.variant || "Default"}`}>{snapshot.variant || "Default"}</span></span><ShellIcon name="down" />
    </Button>
    <Popover placement="top start" className="model-popover" offset={12}>
      <Dialog aria-label="Model settings" className="model-menu-dialog">
        {({ close }) => <><div className="model-menu-close"><Button className="ghost" onClick={close}>Done</Button></div><ModelPicker compact provider={snapshot.provider} model={snapshot.model} variant={snapshot.variant ?? null} busy={busy} onSelect={onSelect} /></>}
      </Dialog>
    </Popover>
  </DialogTrigger>;
}
