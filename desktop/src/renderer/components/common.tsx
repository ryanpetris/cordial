// Small building blocks shared by the pages.
import { useEffect, useId, useRef, type ReactNode } from "react";
import { WarningIcon } from "./icons.tsx";

/** A content page. Its header is a bar level with the window controls that
 * moves the window and holds the page's name, status and actions; the rest
 * scrolls. */
export function Page({
  icon,
  active = true,
  title,
  status,
  actions,
  children,
}: {
  icon: ReactNode;
  /** Whether the icon shows the subject as connected. */
  active?: boolean;
  title: string;
  status?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <>
      <header className="page-header">
        <span className={active ? "page-icon" : "page-icon off"}>{icon}</span>
        <h1 title={title}>{title}</h1>
        {status}
        <span className="page-actions">{actions}</span>
      </header>
      <div className="content-scroll">
        <div className="page">{children}</div>
      </div>
    </>
  );
}

export type Tone = "ok" | "warn" | "low" | "neutral";

/** A short status in a rounded label; `dot` marks a state rather than a reading. */
export function Pill({ tone = "neutral", dot, children }: { tone?: Tone; dot?: boolean; children: ReactNode }) {
  return (
    <span className={`pill ${tone}`}>
      {dot ? <span className="pill-dot" /> : null}
      {children}
    </span>
  );
}

/** A choice among a few options as joined buttons. */
export function Segmented<T extends string | number | boolean>({
  label,
  options,
  value,
  disabled,
  onChange,
}: {
  label: string;
  options: [T, string][];
  value: T | null;
  disabled?: boolean;
  onChange: (value: T) => void;
}) {
  return (
    <span className="segmented" role="group" aria-label={label}>
      {options.map(([v, text]) => (
        <button key={String(v)} aria-pressed={v === value} disabled={disabled} onClick={() => v !== value && onChange(v)}>
          {text}
        </button>
      ))}
    </span>
  );
}

/** Tabs over one panel; the arrow keys move between the enabled tabs. */
export function Tabs<T extends string>({
  label,
  tabs,
  value,
  onChange,
  children,
}: {
  label: string;
  tabs: [T, string, boolean?][];
  value: T;
  onChange: (value: T) => void;
  children: ReactNode;
}) {
  const id = useId();
  const list = useRef<HTMLDivElement>(null);
  const enabled = tabs.filter(([, , disabled]) => !disabled).map(([v]) => v);
  const move = (step: number) => {
    const next = enabled[(enabled.indexOf(value) + step + enabled.length) % enabled.length];
    if (next === undefined || next === value) return;
    onChange(next);
    list.current?.querySelector<HTMLElement>(`[data-tab="${next}"]`)?.focus();
  };
  return (
    <>
      <div
        ref={list}
        className="tabs"
        role="tablist"
        aria-label={label}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
            e.preventDefault();
            move(e.key === "ArrowRight" ? 1 : -1);
          }
        }}
      >
        {tabs.map(([v, text, disabled]) => (
          <button
            key={v}
            id={`${id}-${v}`}
            data-tab={v}
            role="tab"
            className="tab"
            aria-selected={v === value}
            aria-controls={`${id}-panel`}
            tabIndex={v === value ? 0 : -1}
            disabled={disabled}
            onClick={() => onChange(v)}
          >
            {text}
          </button>
        ))}
      </div>
      <div id={`${id}-panel`} role="tabpanel" aria-labelledby={`${id}-${value}`}>
        {children}
      </div>
    </>
  );
}

/** A bar filled to `fraction` of its width. */
export const Meter = ({ fraction, low }: { fraction: number; low?: boolean }) => (
  <span className={low ? "meter low" : "meter"}>
    <span style={{ width: `${Math.round(Math.min(1, Math.max(0, fraction)) * 100)}%` }} />
  </span>
);

export function Card({ title, children, footer }: { title?: string; children: ReactNode; footer?: ReactNode }) {
  return (
    <section className="group">
      {title ? <h2>{title}</h2> : null}
      <div className="card">{children}</div>
      {footer ? <div className="group-footer">{footer}</div> : null}
    </section>
  );
}

export function Row({
  title,
  subtitle,
  children,
  dim,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  children?: ReactNode;
  dim?: boolean;
}) {
  return (
    <div className={dim ? "row dim" : "row"}>
      <div className="row-text">
        <div className="row-title">{title}</div>
        {subtitle ? <div className="row-subtitle">{subtitle}</div> : null}
      </div>
      {children ? <div className="row-end">{children}</div> : null}
    </div>
  );
}

export function Switch({
  checked,
  disabled,
  label,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (value: boolean) => void;
}) {
  return (
    <input
      type="checkbox"
      role="switch"
      className="switch"
      aria-label={label}
      checked={checked}
      disabled={disabled}
      onChange={(e) => onChange(e.target.checked)}
    />
  );
}

/** A labelled switch row; the whole row toggles. */
export function SwitchRow(props: {
  title: string;
  subtitle?: ReactNode;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  const id = useId();
  return (
    <label className={props.disabled ? "row clickable disabled" : "row clickable"} htmlFor={id}>
      <div className="row-text">
        <div className="row-title">{props.title}</div>
        {props.subtitle ? <div className="row-subtitle">{props.subtitle}</div> : null}
      </div>
      <div className="row-end">
        <input
          id={id}
          type="checkbox"
          role="switch"
          className="switch"
          checked={props.checked}
          disabled={props.disabled}
          onChange={(e) => props.onChange(e.target.checked)}
        />
      </div>
    </label>
  );
}

export function Banner({ kind = "info", children, action }: { kind?: "info" | "warning" | "error"; children: ReactNode; action?: ReactNode }) {
  return (
    <div className={`banner ${kind}`} role={kind === "info" ? "status" : "alert"}>
      {kind !== "info" ? <WarningIcon /> : null}
      <div className="banner-text">{children}</div>
      {action}
    </div>
  );
}

export const Spinner = () => <span className="spinner" aria-label="Working" />;

/** A modal dialog on the native <dialog> element. */
export function Dialog({
  open,
  title,
  children,
  onClose,
  className,
}: {
  open: boolean;
  title: string;
  children: ReactNode;
  onClose: () => void;
  className?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className={className ? `dialog ${className}` : "dialog"}
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      {open ? (
        <>
          <header className="dialog-header">
            <h1>{title}</h1>
          </header>
          {children}
        </>
      ) : null}
    </dialog>
  );
}

/** A compact table of labelled values; goes in a card. */
export const Facts = ({ children }: { children: ReactNode }) => <dl className="facts">{children}</dl>;

export function Fact({ label, children, dim }: { label: ReactNode; children: ReactNode; dim?: boolean }) {
  return (
    <div className={dim ? "fact dim" : "fact"}>
      <dt>{label}</dt>
      <dd className="selectable">{children}</dd>
    </div>
  );
}

/** Counts side by side in one card; the first is highlighted. */
export function Summary({ items }: { items: [string, number][] }) {
  return (
    <div className="card summary">
      {items.map(([label, n]) => (
        <div key={label} className="summary-item">
          <span className="summary-value">{n}</span>
          <span className="summary-label">{label}</span>
        </div>
      ))}
    </div>
  );
}
