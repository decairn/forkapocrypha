import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";
import { getZoomAction, nextZoom, parseZoom, type ZoomAction } from "./zoom";

const STORAGE_KEY = "apocrypha:zoom";

export function useAppZoom() {
  const [zoom, setZoom] = useState(() => parseZoom(localStorage.getItem(STORAGE_KEY)));
  const pendingZoom = useRef(Promise.resolve());
  const changeZoom = useCallback((action: ZoomAction) => {
    setZoom((current) => nextZoom(current, action));
  }, []);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, String(zoom));
    // Serialize native calls so rapid key repeats end at the displayed level.
    pendingZoom.current = pendingZoom.current
      .then(() => getCurrentWebview().setZoom(zoom / 100))
      .catch((error) => console.error("Failed to set app zoom:", error));
  }, [zoom]);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      const action = getZoomAction(event);
      if (!action) return;
      event.preventDefault();
      changeZoom(action);
    }

    window.addEventListener("keydown", handleKeyDown, true);
    return () => window.removeEventListener("keydown", handleKeyDown, true);
  }, [changeZoom]);

  return { zoom, changeZoom };
}
