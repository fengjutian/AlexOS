/**
 * VSCode-style horizontal menu bar.
 *
 * Renders the in-page counterpart to the host's native menu (set via
 * `menu.setApplicationMenu`). The same `MenuSpec` tree drives both
 * renderings, so an item added in `lib/menu.ts` shows up in both
 * places without further wiring.
 *
 * Interaction:
 *  - Click a top-level label to toggle its dropdown.
 *  - Click outside the bar to close the open dropdown.
 *  - Press `Escape` to close the open dropdown.
 *  - Click an item (or its accelerator hint) to invoke `run` and
 *    close the dropdown.
 */
import { useEffect, useRef, useState } from "react";
import type React from "react";
import type { MenuItemSpec, MenuSpec } from "../types/desktop.js";

interface MenuBarProps {
  menus: MenuSpec[];
  /** Action runner from `useActionRunner` — keeps the result panel in sync. */
  onRun: (label: string, fn: () => Promise<unknown>) => void;
}

export function MenuBar({ menus, onRun }: MenuBarProps): React.ReactElement {
  const [openId, setOpenId] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);

  // Close on outside click.
  useEffect(() => {
    if (!openId) return undefined;
    const handler = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) {
        setOpenId(null);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [openId]);

  // Close on Escape.
  useEffect(() => {
    if (!openId) return undefined;
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenId(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [openId]);

  const handleSelect = (item: MenuItemSpec) => {
    if (item.type === "separator" || !item.run) return;
    setOpenId(null);
    // Route through the shared runner so the result panel reflects
    // the menu action exactly like a button click.
    onRun(item.label, () => Promise.resolve(item.run!()));
  };

  return (
    <div ref={rootRef} className="menubar" role="menubar" aria-label="主菜单">
      {menus.map((menu) => (
        <div
          key={menu.id}
          className={`menubar__menu${openId === menu.id ? " is-open" : ""}`}
        >
          <button
            type="button"
            className="menubar__top"
            aria-haspopup="menu"
            aria-expanded={openId === menu.id}
            onClick={() => setOpenId((current) => (current === menu.id ? null : menu.id))}
          >
            {menu.label}
          </button>
          {openId === menu.id && (
            <Dropdown items={menu.items} onSelect={handleSelect} />
          )}
        </div>
      ))}
    </div>
  );
}

// ----------------------------------------------------------------------

interface DropdownProps {
  items: MenuItemSpec[];
  onSelect: (item: MenuItemSpec) => void;
}

function Dropdown({ items, onSelect }: DropdownProps): React.ReactElement {
  return (
    <div className="menubar__dropdown" role="menu">
      {items.map((item, index) => {
        if (item.type === "separator") {
          return <div key={`sep-${index}-${item.id}`} className="menubar__sep" role="separator" />;
        }
        return (
          <button
            key={item.id}
            type="button"
            role="menuitem"
            className="menubar__item"
            onClick={() => onSelect(item)}
            title={item.label}
          >
            <span className="menubar__label">{item.label}</span>
            {item.accelerator && <span className="menubar__accel">{item.accelerator}</span>}
          </button>
        );
      })}
    </div>
  );
}
