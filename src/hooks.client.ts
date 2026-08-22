import type { HandleClientError } from "@sveltejs/kit";

/**
 * Surface the real reason when the app fails to boot.
 *
 * Without this hook SvelteKit replaces any uncaught client error with a bare
 * "500 / Internal Error" page. That is close to useless on the desktop shell:
 * there is no devtools window open, and the webview's console never reaches
 * the terminal, so a boot failure shows a dead page and the log says nothing.
 * Reporting the actual error costs nothing and turns a dead end into a fix.
 */
export const handleError: HandleClientError = ({ error, status, message }) => {
  const detail =
    error instanceof Error
      ? `${error.name}: ${error.message}${error.stack ? `\n${error.stack}` : ""}`
      : String(error);

  console.error("[app] unhandled client error", error);

  return {
    // 404s are routing, not faults; keep SvelteKit's own wording for those.
    message: status === 404 ? message : detail,
  };
};
