import { useEffect, useState } from 'react';

// Tailwind's `sm` breakpoint. Below this we collapse data tables and shrink
// chart heights — phone-class viewports.
const MOBILE_QUERY = '(max-width: 639px)';

/** Returns true on phone-sized viewports. Re-renders on viewport changes
 *  (rotate, browser resize). SSR-safe — defaults to false on the server. */
export function useIsMobile(): boolean {
  const [isMobile, setIsMobile] = useState(() => {
    if (typeof window === 'undefined') return false;
    return window.matchMedia(MOBILE_QUERY).matches;
  });

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const mql = window.matchMedia(MOBILE_QUERY);
    const onChange = (e: MediaQueryListEvent) => setIsMobile(e.matches);
    mql.addEventListener('change', onChange);
    return () => mql.removeEventListener('change', onChange);
  }, []);

  return isMobile;
}
