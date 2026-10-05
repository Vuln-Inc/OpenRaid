import type { CSSProperties } from "react";
import { Toaster, toast } from "sonner";

export function notify(message: string) {
  toast.success(message, { id: "desktop-notice" });
}

export function notifyError(message: string) {
  // Coalesce repeated native failures rather than stacking one per event.
  if (message) toast.error(message, { id: "desktop-error", duration: 8000 });
  else toast.dismiss("desktop-error");
}

export function Notifications({ theme }: { theme: "light" | "dark" }) {
  return <Toaster theme={theme} position="bottom-right" closeButton duration={4500} visibleToasts={3}
    containerAriaLabel="Notifications" style={{
      "--normal-bg": "var(--panel)",
      "--normal-text": "var(--text)",
      "--normal-border": "var(--line)",
    } as CSSProperties} toastOptions={{ descriptionClassName: "toast-description" }} />;
}
