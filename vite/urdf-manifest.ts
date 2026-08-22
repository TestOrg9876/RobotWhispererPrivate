import fs from "node:fs";
import path from "node:path";

import type { Plugin } from "vite";

function humanize(directory: string): string {
  return directory.replace(/_/g, " ").replace(/\b\w/g, (character) => character.toUpperCase());
}

export default function urdfManifest(): Plugin {
  const assetsDir = path.resolve("static/assets");
  const manifestPath = path.join(assetsDir, "manifest.json");

  function generateManifest(): void {
    if (!fs.existsSync(assetsDir)) return;
    const entries = [];
    // Sorted, because `readdirSync` returns directory order, which differs
    // between filesystems and machines. Without this the committed
    // manifest.json is rewritten on almost every checkout and shows up as a
    // spurious diff in unrelated changes.
    const directories = fs
      .readdirSync(assetsDir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
      .sort();
    for (const name of directories) {
      const urdf = fs
        .readdirSync(path.join(assetsDir, name))
        .sort()
        .find((file) => file.endsWith(".urdf"));
      if (urdf) entries.push({ name: humanize(name), directory: name, urdf });
    }
    const next = JSON.stringify(entries, null, 2) + "\n";
    if (fs.existsSync(manifestPath) && fs.readFileSync(manifestPath, "utf-8") === next) return;
    fs.writeFileSync(manifestPath, next);
  }

  return {
    name: "urdf-manifest",
    buildStart() {
      generateManifest();
    },
    configureServer(server) {
      server.watcher.add(assetsDir);
      server.watcher.on("all", (_event, filePath) => {
        if (filePath.startsWith(assetsDir) && filePath !== manifestPath) generateManifest();
      });
      generateManifest();
    },
  };
}
