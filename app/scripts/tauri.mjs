import { run } from "@tauri-apps/cli";
import { distributionConfig } from "./distribution.mjs";

const args = process.argv.slice(2);
try {
  if (["dev", "build", "bundle"].includes(args[0])) {
    const { updater } = distributionConfig();
    const separator = args.indexOf("--");
    args.splice(
      separator === -1 ? args.length : separator,
      0,
      "--config",
      JSON.stringify({ plugins: { updater } }),
    );
  }
  await run(args, "npm run tauri");
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
