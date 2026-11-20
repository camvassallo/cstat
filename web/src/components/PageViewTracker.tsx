import { useEffect } from 'react';
import { useLocation } from 'react-router-dom';
import { trackPageView } from '../lib/analytics';

// Most routes are lazy chunks and set their title after a fetch (a player page
// is "Camalytics" until the player loads), so a page view sent the moment the
// route changes would file nearly every page under the site name. Wait for the
// title to settle; navigating away first sends the pending view immediately so
// a quick click-through still counts.
const SETTLE_MS = 1500;

// One page view per PATH. Query-string changes (season picker, table filters,
// tabs) are interactions on the same page, not new pages; the full URL still
// goes up as page_location.
export default function PageViewTracker() {
  const { pathname } = useLocation();
  useEffect(() => {
    // Snapshot what belongs to THIS page. By the time the cleanup runs the
    // router has already moved to the next URL, and the next page may have set
    // its title, so neither can be read then.
    const href = window.location.href;
    let title = document.title;
    const sample = window.setInterval(() => {
      title = document.title;
    }, 250);
    let sent = false;
    const send = () => {
      if (sent) return;
      sent = true;
      window.clearInterval(sample);
      trackPageView(pathname, href, title);
    };
    const t = window.setTimeout(() => {
      title = document.title;
      send();
    }, SETTLE_MS);
    // Closing the tab inside the settle window would otherwise send nothing,
    // and a bounce would leave no page view at all.
    const onHide = () => send();
    window.addEventListener('pagehide', onHide);
    return () => {
      window.clearTimeout(t);
      window.removeEventListener('pagehide', onHide);
      // React StrictMode (dev) unmounts and remounts every effect once on the
      // same URL. Only a real navigation flushes; a remount on the same path
      // drops this pending view and the next mount schedules its own.
      if (window.location.pathname === pathname) {
        window.clearInterval(sample);
        return;
      }
      send();
    };
  }, [pathname]);
  return null;
}
