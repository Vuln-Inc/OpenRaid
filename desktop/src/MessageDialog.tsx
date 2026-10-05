import { useId } from "react";
import type { Message } from "./bridge";
import { Dialog, Modal, ModalOverlay } from "./components/application/modals/modal";
import { Button } from "./ui";

export function MessageDialog({ message, onClose }: { message: Message; onClose: () => void }) {
  const id = useId();
  return <ModalOverlay isOpen onOpenChange={open => { if (!open) onClose(); }} isDismissable>
    <Modal className="message-dialog max-w-4xl"><Dialog aria-labelledby={id}>
      <div className="subtoolbar"><h2 id={id}>{message.sender} · #{message.seq}</h2><Button autoFocus onClick={onClose}>Close</Button></div>
      <pre className="px-6 pb-6">{message.body}</pre>
    </Dialog></Modal>
  </ModalOverlay>;
}
