import assert from "node:assert/strict";
import test from "node:test";
import { distributionConfig } from "../scripts/distribution.mjs";

const upstream = "https://github.com/VitorTaichou/apocrypha";
const fork = "https://github.com/decairn/forkapocrypha";

test("the original build uses the original feed and a single About link", () => {
  const { updater, aboutLinks } = distributionConfig({
    GITHUB_REPOSITORY: "VitorTaichou/apocrypha",
  });

  assert.deepEqual(updater.endpoints, [`${upstream}/releases/latest/download/latest.json`]);
  assert.ok(updater.pubkey, "preserve the original public key");
  assert.deepEqual(aboutLinks.map(({ url }) => url), [upstream]);
});

test("the fork uses its own feed and key, with the original credited second", () => {
  const { updater, aboutLinks } = distributionConfig({
    GITHUB_REPOSITORY: "decairn/forkapocrypha",
    TAURI_SIGNING_PUBLIC_KEY: "fork-public-key",
  });

  assert.deepEqual(updater.endpoints, [`${fork}/releases/latest/download/latest.json`]);
  assert.equal(updater.pubkey, "fork-public-key");
  assert.deepEqual(aboutLinks.map(({ url }) => url), [fork, upstream]);
  assert.equal(aboutLinks[1].label, "Original project by VitorTaichou");
});

test("an unsigned fork build never inherits the original signing key", () => {
  const env = { GITHUB_REPOSITORY: "decairn/forkapocrypha" };
  assert.equal(distributionConfig(env).updater.pubkey, "");
  assert.throws(
    () => distributionConfig({ ...env, TAURI_SIGNING_PRIVATE_KEY: "test-key" }),
    /TAURI_SIGNING_PUBLIC_KEY/,
  );
});
