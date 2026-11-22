import assert from "node:assert/strict";
import test from "node:test";
import { getZoomAction, nextZoom, parseZoom } from "../src/lib/zoom.ts";

function shortcut(key, modifiers = {}) {
  return getZoomAction({ key, ctrlKey: true, altKey: false, metaKey: false, isComposing: false, ...modifiers });
}

test("maps the desktop shortcuts, including shifted and numpad plus", () => {
  assert.equal(shortcut("+", { code: "NumpadAdd" }), "in");
  assert.equal(shortcut("+", { code: "Equal", shiftKey: true }), "in");
  assert.equal(shortcut("="), "in");
  assert.equal(shortcut("-"), "out");
  assert.equal(shortcut("0"), "reset");
  assert.equal(shortcut("u"), undefined);
  assert.equal(shortcut("=", { ctrlKey: false }), undefined);
  assert.equal(shortcut("=", { altKey: true }), undefined);
  assert.equal(shortcut("=", { metaKey: true }), undefined);
  assert.equal(shortcut("=", { isComposing: true }), undefined);
});

test("steps up and down, preserves bounds, and resets to 100%", () => {
  assert.equal(nextZoom(100, "in"), 110);
  assert.equal(nextZoom(100, "out"), 90);
  assert.equal(nextZoom(150, "in"), 150);
  assert.equal(nextZoom(80, "out"), 80);
  assert.equal(nextZoom(150, "reset"), 100);
});

test("restores supported percentages and defaults invalid saved values", () => {
  for (const value of [80, 100, 130, 150]) {
    assert.equal(parseZoom(String(value)), value);
  }
  for (const value of [null, "", "NaN", "120%", "79", "95", "160"]) {
    assert.equal(parseZoom(value), 100);
  }
});
