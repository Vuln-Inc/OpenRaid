import { Button as UntitledButton, type ButtonProps } from "./components/base/buttons/button";
import { Select } from "./components/base/select/select";

/** Native-compatible adapter; the rendered control is Untitled UI's Button. */
export function Button({ disabled, className = "", type = "button", title, ...props }: Omit<ButtonProps, "className"> & { disabled?: boolean; className?: string; title?: string }) {
  const card = /\b(agent-row|message|theme-option|saved-session-card)\b/.test(className);
  const cardLayout = className.includes("message") ? "block" : "flex flex-col items-stretch justify-start";
  const listRow = /\b(agent-row|message)\b/.test(className);
  const color = className.includes("primary") ? "primary" : className.includes("danger") ? "secondary-destructive" : card || className.includes("ghost") ? "tertiary" : "secondary";
  return <UntitledButton {...props} {...{ title }} aria-description={title} type={type} isDisabled={disabled} color={color} noTextPadding={card}
    className={`desktop-button ${card ? `${cardLayout} h-full w-full whitespace-normal text-left font-normal shadow-none ring-0 [&>[data-text]]:contents ${listRow ? "rounded-none" : ""}` : color === "primary" ? "text-[var(--background)] disabled:opacity-100 disabled:bg-secondary disabled:text-secondary disabled:ring-primary" : ""} ${className}`} />;
}

export interface SelectOption { id: string; label: string; disabled?: boolean }
export function SearchSelect({ label, value, options, onChange, disabled, hint, placeholder }: {
  label: string; value: string; options: SelectOption[]; onChange: (id: string) => void;
  disabled?: boolean; hint?: string; placeholder?: string;
}) {
  return <Select.ComboBox label={label} placeholder={placeholder ?? `Search ${label.toLowerCase()}…`} shortcut={false}
    selectedKey={value || null} onSelectionChange={key => { if (key !== null) onChange(String(key)); }}
    defaultItems={options} disabledKeys={options.filter(item => item.disabled).map(item => item.id)}
    isDisabled={disabled} hint={hint} className="min-w-0" allowsEmptyCollection>
    {item => <Select.Item id={item.id} textValue={item.label}>{item.label}</Select.Item>}
  </Select.ComboBox>;
}
