import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const original = "VitorTaichou/apocrypha";
const originalUrl = `https://github.com/${original}`;
const tauriConfig = JSON.parse(
  readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);

function gitHubOrigin() {
  const { stdout } = spawnSync("git", ["remote", "get-url", "origin"], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    encoding: "utf8",
  });
  return stdout?.trim()
    .match(/^(?:https?:\/\/github\.com\/|ssh:\/\/git@github\.com\/|git@github\.com:)([^/]+\/[^/]+)\/?$/i)?.[1]
    ?.replace(/\.git$/i, "");
}

export function distributionConfig(env = process.env) {
  const repository = env.GITHUB_REPOSITORY || gitHubOrigin() || original;
  if (!/^[a-z0-9-]+\/(?!\.{1,2}$)[a-z0-9_.-]+$/i.test(repository)) {
    throw new Error("GITHUB_REPOSITORY must be a GitHub owner/repository name.");
  }

  const isOriginal = repository.toLowerCase() === original.toLowerCase();
  const url = `https://github.com/${isOriginal ? original : repository}`;
  const pubkey = env.TAURI_SIGNING_PUBLIC_KEY?.trim() ||
    (isOriginal ? tauriConfig.plugins.updater.pubkey : "");
  if (!pubkey && env.TAURI_SIGNING_PRIVATE_KEY) {
    throw new Error("Set TAURI_SIGNING_PUBLIC_KEY to the public key for this distribution.");
  }

  const aboutLinks = [
    { url, label: isOriginal ? "View on GitHub" : "View distribution on GitHub" },
  ];
  if (!isOriginal) {
    aboutLinks.push({ url: originalUrl, label: "Original project by VitorTaichou" });
  }

  return {
    updater: { pubkey, endpoints: [`${url}/releases/latest/download/latest.json`] },
    aboutLinks,
  };
}
