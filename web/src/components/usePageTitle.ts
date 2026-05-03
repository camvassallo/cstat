import { useEffect } from 'react';

const SITE = 'CamPom';

/** Set the browser tab title. Pass `null` to keep the bare site name (used
 *  while data is loading on dynamic pages so the tab doesn't flicker through
 *  a blank or partial state). On unmount, resets to the site name. */
export function usePageTitle(title: string | null): void {
  useEffect(() => {
    document.title = title ? `${title} — ${SITE}` : SITE;
    return () => {
      document.title = SITE;
    };
  }, [title]);
}
