/**
 * VSCode-style horizontal menu bar with nested submenus.
 *
 * Renders the in-page counterpart to the host's native menu (set via
 * `menu.setApplicationMenu`). The same `MenuSpec` tree drives both
 * renderings, so an item added in `lib/menu.ts` shows up in both
 * places without further wiring.
 *
 * The top-level dropdowns are rendered through a React portal into
 * `document.body` and positioned with `position: fixed` — that way
 * they escape the menubar's `overflow-x: auto` wrapper (the spec
 * forces `overflow-y: visible` to behave like `auto` whenever the
 * sibling axis is `auto`, so any container that scrolls clips the
 * dropdown). Nested submenus continue to live inside their parent
 * dropdown because they only need to fly out horizontally and never
 * cross a scroll boundary.
 *
 * Interaction model — matches VSCode's menubar:
 *  - Click a top-level label to toggle its dropdown.
 *  - Hover an item with a submenu to fly the submenu out to the right.
 *  - Click an item to invoke `run` and close the entire menu tree.
 *  - Click outside, or press `Escape`, to close everything.
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type React from "react";
import type { MenuItemSpec, MenuSpec } from "../types/desktop.js";

interface MenuBarProps {
  menus: MenuSpec[];
  /** Action runner from `useActionRunner` — keeps the result panel in sync. */
  onRun: (label: string, fn: () => Promise<unknown>) => void;
}

export function MenuBar({ menus, onRun }: MenuBarProps): React.ReactElement {
  const [openId, setOpenId] = useState<string | null>(null);
  // `submenuPath` is the list of item ids currently expanded on the
  // open menu, e.g. `["file.open", "file.open.recent"]`. Only one
  // submenu can be open per level, so a path list models the cascade.
  const [submenuPath, setSubmenuPath] = useState<string[]>([]);
  // Position of the open top-level dropdown, captured at click time
  // from the trigger button's bounding rect. Kept in state so a
  // resize / scroll can refresh it without re-measuring every render.
  const [openPos, setOpenPos] = useState<{ top: number; left: number } | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttonRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  // Recompute the dropdown position on resize / scroll so it stays
  // pinned under its trigger even after the menubar moves.
  useLayoutEffect(() => {
    if (!openId) return undefined;
    const update = () => {
      const btn = buttonRefs.current[openId];
      if (!btn) return;
      const rect = btn.getBoundingClientRect();
      setOpenPos({ top: rect.bottom, left: rect.left });
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, { passive: true });
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update);
    };
  }, [openId]);

  // Close on outside click.
  useEffect(() => {
    if (!openId) return undefined;
    const handler = (event: MouseEvent) => {
      // The portal renders the dropdown outside `rootRef`, so the
      // contains-check has to include both the menubar and the
      // dropdown's own DOM node.
      const target = event.target as Node;
      const insideBar = rootRef.current?.contains(target) ?? false;
      const dropdown = document.querySelector(".menubar__dropdown--top");
      const insideDropdown = dropdown?.contains(target) ?? false;
      if (!insideBar && !insideDropdown) {
        setOpenId(null);
        setSubmenuPath([]);
        setOpenPos(null);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [openId]);

  // Close on Escape.
  useEffect(() => {
    if (!openId) return undefined;
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenId(null);
        setSubmenuPath([]);
        setOpenPos(null);
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [openId]);

  const handleSelect = (item: MenuItemSpec) => {
    if (item.type === "separator" || !item.run) return;
    setOpenId(null);
    setSubmenuPath([]);
    setOpenPos(null);
    onRun(item.label, () => Promise.resolve(item.run!()));
  };

  const handleTopClick = (id: string) => {
    setOpenId((current) => {
      if (current === id) {
        setSubmenuPath([]);
        setOpenPos(null);
        return null;
      }
      setSubmenuPath([]);
      // Capture the position synchronously so the dropdown appears
      // at the right place on the very first frame.
      const btn = buttonRefs.current[id];
      if (btn) {
        const rect = btn.getBoundingClientRect();
        setOpenPos({ top: rect.bottom, left: rect.left });
      }
      return id;
    });
  };

  // The portal target: only render the dropdown on the client (the
  // document is undefined during SSR) and only when something is open.
  const portalTarget = typeof document !== "undefined" ? document.body : null;
  const openMenu = openId ? menus.find((m) => m.id === openId) : null;

  return (
    <div ref={rootRef} className="menubar-wrap" role="presentation">
      <div className="menubar" role="menubar" aria-label="主菜单">
        {menus.map((menu) => (
          <div
            key={menu.id}
            className={`menubar__menu${openId === menu.id ? " is-open" : ""}`}
          >
            <button
              ref={(el) => {
                buttonRefs.current[menu.id] = el;
              }}
              type="button"
              className="menubar__top"
              aria-haspopup="menu"
              aria-expanded={openId === menu.id}
              onClick={() => handleTopClick(menu.id)}
            >
              {menu.label}
            </button>
          </div>
        ))}
      </div>
      {portalTarget && openMenu && openPos &&
        createPortal(
          <TopLevelDropdown
            menu={openMenu}
            position={openPos}
            onSelect={(item) => handleSelect(item)}
            submenuPath={submenuPath}
            setSubmenuPath={setSubmenuPath}
          />,
          portalTarget,
        )}
    </div>
  );
}

// ----------------------------------------------------------------------

interface TopLevelDropdownProps {
  menu: MenuSpec;
  position: { top: number; left: number };
  onSelect: (item: MenuItemSpec) => void;
  submenuPath: string[];
  setSubmenuPath: React.Dispatch<React.SetStateAction<string[]>>;
}

/**
 * The top-level dropdown is rendered through a portal and pinned
 * with `position: fixed` so it can hang below the menubar without
 * being clipped by the menubar's overflow.
 */
function TopLevelDropdown({
  menu,
  position,
  onSelect,
  submenuPath,
  setSubmenuPath,
}: TopLevelDropdownProps): React.ReactElement {
  return (
    <div
      className="menubar__dropdown menubar__dropdown--top"
      role="menu"
      style={{ top: position.top, left: position.left }}
    >
      {menu.items.map((item, index) => (
        <Row
          key={item.id}
          item={item}
          index={index}
          parentPath={[menu.id]}
          onSelect={onSelect}
          submenuPath={submenuPath}
          setSubmenuPath={setSubmenuPath}
        />
      ))}
    </div>
  );
}

interface RowProps {
  item: MenuItemSpec;
  index: number;
  parentPath: string[];
  onSelect: (item: MenuItemSpec) => void;
  submenuPath: string[];
  setSubmenuPath: React.Dispatch<React.SetStateAction<string[]>>;
}

function Row({
  item,
  index,
  parentPath,
  onSelect,
  submenuPath,
  setSubmenuPath,
}: RowProps): React.ReactElement | null {
  const rowRef = useRef<HTMLDivElement | null>(null);
  const [subPos, setSubPos] = useState<{ top: number; left: number } | null>(null);

  if (item.type === "separator") {
    return <div key={`sep-${index}-${item.id}`} className="menubar__sep" role="separator" />;
  }

  const hasSubmenu = !!item.items && item.items.length > 0;
  const isSubmenuOpen = submenuPath[parentPath.length] === item.id;

  // When the submenu becomes open, capture its anchor position from
  // the row's bounding rect so the sub-dropdown flies out to the right.
  useLayoutEffect(() => {
    if (!isSubmenuOpen || !rowRef.current) return;
    const rect = rowRef.current.getBoundingClientRect();
    setSubPos({ top: rect.top, left: rect.right });
  }, [isSubmenuOpen]);

  return (
    <div ref={rowRef} className="menubar__row">
      <button
        type="button"
        role="menuitem"
        aria-haspopup={hasSubmenu ? "menu" : undefined}
        aria-expanded={hasSubmenu ? isSubmenuOpen : undefined}
        className={`menubar__item${isSubmenuOpen ? " is-submenu-open" : ""}`}
        onClick={() => {
          if (hasSubmenu) {
            setSubmenuPath((current) => {
              const next = current.slice(0, parentPath.length);
              next.push(item.id);
              return next;
            });
          } else {
            onSelect(item);
          }
        }}
        onMouseEnter={() => {
          if (hasSubmenu) {
            setSubmenuPath((current) => {
              const next = current.slice(0, parentPath.length);
              next.push(item.id);
              return next;
            });
          }
        }}
        title={item.label}
      >
        <span className="menubar__label">{item.label}</span>
        {item.accelerator && <span className="menubar__accel">{item.accelerator}</span>}
        {hasSubmenu && <span className="menubar__chevron" aria-hidden="true">▸</span>}
      </button>
      {hasSubmenu && isSubmenuOpen && subPos && (
        <SubDropdown
          items={item.items!}
          anchor={subPos}
          parentPath={[...parentPath, item.id]}
          onSelect={onSelect}
          submenuPath={submenuPath}
          setSubmenuPath={setSubmenuPath}
        />
      )}
    </div>
  );
}

interface SubDropdownProps {
  items: MenuItemSpec[];
  anchor: { top: number; left: number };
  parentPath: string[];
  onSelect: (item: MenuItemSpec) => void;
  submenuPath: string[];
  setSubmenuPath: React.Dispatch<React.SetStateAction<string[]>>;
}

/**
 * Sub-dropdown lives inside the top-level dropdown (which is itself
 * portalled) and is pinned with `position: fixed` so it can fly out
 * to the right past the parent panel.
 */
function SubDropdown({
  items,
  anchor,
  parentPath,
  onSelect,
  submenuPath,
  setSubmenuPath,
}: SubDropdownProps): React.ReactElement {
  return (
    <div
      className="menubar__dropdown menubar__dropdown--sub"
      role="menu"
      style={{ top: anchor.top, left: anchor.left }}
    >
      {items.map((item, index) => (
        <Row
          key={item.id}
          item={item}
          index={index}
          parentPath={parentPath}
          onSelect={onSelect}
          submenuPath={submenuPath}
          setSubmenuPath={setSubmenuPath}
        />
      ))}
    </div>
  );
}
