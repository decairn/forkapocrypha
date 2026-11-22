import { useLayoutEffect, useRef } from "react";

const MENU_GAP = 6;
const VIEWPORT_MARGIN = 8;
const MAX_MENU_HEIGHT = 420;

export function useDropdownFit(open: boolean) {
  const menuRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!open) return;
    const menu = menuRef.current;
    const anchor = menu?.parentElement;
    if (!menu || !anchor) return;

    const updatePosition = () => {
      const { top, bottom } = anchor.getBoundingClientRect();
      const above = top - MENU_GAP - VIEWPORT_MARGIN;
      const below = window.innerHeight - bottom - MENU_GAP - VIEWPORT_MARGIN;
      const menuHeight = Math.min(MAX_MENU_HEIGHT, menu.scrollHeight + 2);
      const openAbove = below < menuHeight && above > below;
      const offset = `calc(100% + ${MENU_GAP}px)`;
      menu.style.top = openAbove ? "auto" : offset;
      menu.style.bottom = openAbove ? offset : "auto";
      menu.style.maxHeight = `${Math.max(0, Math.min(MAX_MENU_HEIGHT, openAbove ? above : below))}px`;
    };

    updatePosition();
    window.addEventListener("resize", updatePosition);
    return () => window.removeEventListener("resize", updatePosition);
  }, [open]);

  return menuRef;
}
