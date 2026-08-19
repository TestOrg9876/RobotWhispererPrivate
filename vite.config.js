import path from "node:path";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";

import urdfManifest from "./vite/urdf-manifest";
import meshOptimize from "./vite/mesh-optimize";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Desktop builds redirect the wasm-pack specifier to a stub so the bundler
// resolves cleanly even when `src/lib/wasm/generated/` does not exist.
// SvelteKit's `$lib` resolver runs first and rewrites `$lib/wasm/...` to an
// absolute on-disk path, so the alias has to match on the resolved path rather
// than the original `$lib/...` specifier. Only the `web`/`build` scripts set
// `RW_TARGET=web`; everything else (electron dev/build, plain `dev`) is desktop.
const isWebTarget = process.env.RW_TARGET === "web";
const wasmGeneratedPath = path.resolve(__dirname, "src/lib/wasm/generated/rw_wasm");
const wasmStubPath = path.resolve(__dirname, "src/lib/wasm/stub.ts");

/** @type {import('vite').Plugin | null} */
const rwDesktopWasmStubPlugin = isWebTarget
  ? null
  : {
      name: "rw-desktop-wasm-stub",
      enforce: "pre",
      resolveId(source) {
        if (
          source === "$lib/wasm/generated/rw_wasm" ||
          source === wasmGeneratedPath ||
          source.endsWith("/wasm/generated/rw_wasm")
        ) {
          return wasmStubPath;
        }
        return null;
      },
    };

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    rwDesktopWasmStubPlugin,
    urdfManifest(),
    meshOptimize(),
    tailwindcss(),
    sveltekit(),
  ].filter(Boolean),
  // Build-target constant. `import.meta.env.RW_WEB` is `true` only for the web
  // shell and `false` for the Electron desktop shell. The RPC dispatch branches
  // on this compile-time constant so Vite dead-code-eliminates the wrong
  // implementation per build: the desktop bundle never imports the WASM module,
  // and the web bundle never imports the daemon client.
  define: {
    "import.meta.env.RW_WEB": JSON.stringify(isWebTarget),
  },
  // Absolute asset paths (`/_app/...`) are fine under the shell's `app://`
  // origin: the custom scheme is registered as `standard`, so it keeps a host
  // and root-relative URLs resolve against `app://bundle/` as they would on a
  // web server. This is one of the reasons the renderer is not on `file://`.
  clearScreen: false,
  server: {
    port: 5173,
    watch: {
      ignored: ["**/core/**", "**/electron/**", "**/dist-electron/**"],
    },
  },
}));
