/**
 * Runtime platform detection.
 *
 * The desktop shell is identified by the `__RW_NATIVE__` bridge that the
 * Electron preload installs, replacing the old `__TAURI_INTERNALS__` sniff.
 * Note this is a *runtime* check; the build-target switch is the compile-time
 * `import.meta.env.RW_WEB` constant, which is what dead-code-eliminates the
 * unused backend.
 */
export const isDesktop = (): boolean => typeof window !== "undefined" && "__RW_NATIVE__" in window;

export const isBrowser = (): boolean => typeof window !== "undefined" && !isDesktop();

type UaDataNavigator = Navigator & { userAgentData?: { mobile?: boolean } };

export const isMobileDevice = (): boolean => {
  if (typeof navigator === "undefined") return false;
  if (isDesktop()) return false;

  const ua = navigator.userAgent;

  if (/iPhone|iPod/i.test(ua)) return true;
  if (/Android/i.test(ua)) return /\bMobile\b/i.test(ua);

  const uaData = (navigator as UaDataNavigator).userAgentData;
  if (uaData?.mobile === true) return true;

  return /webOS|BlackBerry|IEMobile|Opera Mini|Windows Phone/i.test(ua);
};
