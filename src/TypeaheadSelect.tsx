import { useEffect, useId, useMemo, useRef, useState } from "react";

export type TypeaheadOption = {
  value: string;
  label: string;
  origin: string;
  detail?: string;
  search: string;
};

export function TypeaheadSelect({
  value,
  options,
  onChange,
  ariaLabel,
}: {
  value: string;
  options: TypeaheadOption[];
  onChange: (value: string) => void;
  ariaLabel?: string;
}) {
  const listId = useId();
  const root = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const selected = options.find(option => option.value === value);
  const closedLabel = selected ? `${selected.label} · ${selected.origin}` : "";
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState(closedLabel);
  const [highlight, setHighlight] = useState(0);
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!open || !needle || needle === closedLabel.toLowerCase()) return options;
    return options.filter(option => option.search.toLowerCase().includes(needle));
  }, [closedLabel, open, options, query]);

  useEffect(() => {
    if (!open) setQuery(closedLabel);
  }, [closedLabel, open]);

  useEffect(() => {
    setHighlight(0);
  }, [query, open]);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    return () => document.removeEventListener("mousedown", onPointer);
  }, [open]);

  const choose = (next: string) => {
    onChange(next);
    setOpen(false);
    input.current?.blur();
  };

  return <div className={`typeahead${open ? " open" : ""}`} ref={root}>
    <input
      ref={input}
      role="combobox"
      aria-label={ariaLabel}
      aria-expanded={open}
      aria-controls={listId}
      aria-autocomplete="list"
      value={open ? query : closedLabel}
      onFocus={event => { setOpen(true); setQuery(""); event.target.select(); }}
      onChange={event => { setOpen(true); setQuery(event.target.value); }}
      onKeyDown={event => {
        if (event.key === "ArrowDown") {
          event.preventDefault();
          setOpen(true);
          setHighlight(index => Math.min(index + 1, Math.max(filtered.length - 1, 0)));
        } else if (event.key === "ArrowUp") {
          event.preventDefault();
          setHighlight(index => Math.max(index - 1, 0));
        } else if (event.key === "Enter") {
          event.preventDefault();
          const option = filtered[highlight];
          if (option) choose(option.value);
        } else if (event.key === "Escape" || event.key === "Tab") {
          setOpen(false);
        }
      }}
    />
    {open && <div className="typeahead-list" role="listbox" id={listId}>
      {filtered.length ? filtered.map((option, index) => <button
        type="button"
        role="option"
        aria-selected={option.value === value}
        aria-label={`${option.label} ${option.origin}`}
        className={`typeahead-option${index === highlight ? " active" : ""}`}
        key={option.value}
        onMouseEnter={() => setHighlight(index)}
        onMouseDown={event => event.preventDefault()}
        onClick={() => choose(option.value)}
      >
        <strong>{option.label}</strong>
        <span>{option.origin}{option.detail ? ` · ${option.detail}` : ""}</span>
      </button>) : <div className="typeahead-empty">No matching models</div>}
    </div>}
  </div>;
}
