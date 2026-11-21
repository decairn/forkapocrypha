import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown, type LucideIcon } from "lucide-react";

interface SearchSelectOption<Key extends string> {
  key: Key;
  label: string;
  icon: LucideIcon;
}

interface SearchSelectProps<Key extends string> {
  label: string;
  value: Key;
  onChange: (key: Key) => void;
  options: [SearchSelectOption<Key>, ...SearchSelectOption<Key>[]];
  widthClass: string;
}

export function SearchSelect<Key extends string>({
  label,
  value,
  onChange,
  options,
  widthClass,
}: SearchSelectProps<Key>) {
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", onClick);
    return () => {
      document.removeEventListener("mousedown", onClick);
    };
  }, [open]);

  const current = options.find((o) => o.key === value) ?? options[0];
  const CurrentIcon = current.icon;

  return (
    <div
      ref={wrapperRef}
      className={`relative shrink-0 ${widthClass}`}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) setOpen(false);
      }}
      onKeyDown={(e) => {
        if (open && e.key === "Escape") {
          setOpen(false);
          triggerRef.current?.focus();
        }
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        aria-label={`${label}: ${current.label}`}
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center justify-between gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] py-3.5 pl-4 pr-3 text-[var(--color-on-surface)] hover:border-[var(--color-outline-variant)] focus:border-[var(--color-primary)] focus:outline-none focus:ring-1 focus:ring-[var(--color-primary)]"
      >
        <span className="flex min-w-0 items-center gap-2.5">
          <CurrentIcon
            className="h-4 w-4 shrink-0 text-[var(--color-on-surface-variant)]"
            strokeWidth={1.75}
          />
          <span className="truncate text-sm leading-6">{current.label}</span>
        </span>
        <ChevronDown
          className={`h-4 w-4 shrink-0 text-[var(--color-outline)] transition-transform ${
            open ? "rotate-180" : ""
          }`}
          strokeWidth={1.75}
        />
      </button>

      {open ? (
        <div className="absolute left-0 right-0 top-full z-20 mt-1.5 overflow-hidden rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] p-1 shadow-2xl shadow-black/60">
          {options.map((opt) => {
            const Icon = opt.icon;
            const active = opt.key === value;
            return (
              <button
                key={opt.key}
                type="button"
                aria-pressed={active}
                onClick={() => {
                  onChange(opt.key);
                  setOpen(false);
                  triggerRef.current?.focus();
                }}
                className={`flex w-full items-center gap-3 rounded px-3 py-2 text-left text-sm transition-colors ${
                  active
                    ? "bg-[var(--color-surface-container)] text-[var(--color-on-surface)]"
                    : "text-[var(--color-on-surface-variant)] hover:bg-[var(--color-surface-low)] hover:text-[var(--color-on-surface)]"
                }`}
              >
                <Icon className="h-4 w-4 shrink-0" strokeWidth={1.75} />
                <span className="flex-1">{opt.label}</span>
                {active ? (
                  <Check
                    className="h-3.5 w-3.5 shrink-0 text-[var(--color-primary)]"
                    strokeWidth={2}
                  />
                ) : null}
              </button>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
