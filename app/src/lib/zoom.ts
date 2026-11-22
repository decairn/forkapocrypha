export const DEFAULT_ZOOM = 100;
export const MIN_ZOOM = 80;
export const MAX_ZOOM = 150;
const ZOOM_STEP = 10;

export type ZoomAction = "in" | "out" | "reset";

export function parseZoom(value: string | null): number {
  const zoom = Number(value);
  return zoom >= MIN_ZOOM && zoom <= MAX_ZOOM && zoom % ZOOM_STEP === 0
    ? zoom
    : DEFAULT_ZOOM;
}

export function nextZoom(zoom: number, action: ZoomAction): number {
  if (action === "reset") return DEFAULT_ZOOM;
  const step = action === "in" ? ZOOM_STEP : -ZOOM_STEP;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom + step));
}

export function getZoomAction(
  event: Pick<KeyboardEvent, "key" | "ctrlKey" | "altKey" | "metaKey" | "isComposing">,
): ZoomAction | undefined {
  if (!event.ctrlKey || event.altKey || event.metaKey || event.isComposing) return;
  if (event.key === "+" || event.key === "=") return "in";
  if (event.key === "-") return "out";
  if (event.key === "0") return "reset";
}
