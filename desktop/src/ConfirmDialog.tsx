import { useId } from "react";
import { Dialog, Modal, ModalOverlay } from "./components/application/modals/modal";
import { Button } from "./ui";

export function ConfirmDialog({ title, description, confirmLabel, onConfirm, onClose, cancelLabel = "Cancel", confirmClassName = "danger", onCancel }: { title: string; description: string; confirmLabel: string; onConfirm: () => void; onClose: () => void; cancelLabel?: string; confirmClassName?: string; onCancel?: () => void }) {
  const id = useId();
  const cancel = () => { onClose(); onCancel?.(); };
  return <ModalOverlay isOpen onOpenChange={open => { if (!open) cancel(); }} isDismissable>
    <Modal className="confirm-dialog max-w-md"><Dialog aria-labelledby={`${id}-title`} aria-describedby={`${id}-description`} className="p-6">
      <h2 id={`${id}-title`}>{title}</h2><p id={`${id}-description`} className="mt-3 mb-6 text-sm text-tertiary leading-relaxed">{description}</p>
      <div className="dialog-actions"><Button autoFocus onClick={cancel}>{cancelLabel}</Button><Button className={confirmClassName} onClick={() => { onClose(); onConfirm(); }}>{confirmLabel}</Button></div>
    </Dialog></Modal>
  </ModalOverlay>;
}
