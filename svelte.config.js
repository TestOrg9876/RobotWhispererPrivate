// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
    serviceWorker: {
      // Register the service worker for the web build only.
      //
      // In the desktop shell the frontend is served by Tauri's custom
      // protocol, not by http(s). Service workers are not available on a
      // custom scheme, so SvelteKit's automatic registration throws during
      // boot and the app dies on SvelteKit's generic "500 Internal Error"
      // page before anything renders.
      //
      // The worker exists to cache robot meshes for the browser build, which
      // the desktop build has no use for anyway: its assets are embedded in
      // the binary.
      register: process.env.RW_TARGET === "web",
    },
  },
};

export default config;
