import { Component, useEffect, type ErrorInfo, type ReactNode } from 'react';
import { reportCaughtError, routePattern } from '../lib/errorReporter';

// One-shot reload guard, in sessionStorage so a chunk that is genuinely missing
// (rather than merely stale) can't put the tab in a reload loop.
//
// Keyed PER ROUTE. A single global flag was not actually one-shot: `React.lazy`
// caches a rejected import forever, so once a chunk is known-missing, visiting
// any working route in between cleared the flag and the next visit to the
// broken one spent another whole page reload, discarding in-page state each
// time. Per-route, a route that has already burned its reload stays burned.
const RELOAD_KEY_PREFIX = 'cstat:chunk-reload:';
const reloadKey = (pathname: string) => RELOAD_KEY_PREFIX + routePattern(pathname);

// A failed `React.lazy` import surfaces as a TypeError whose message varies by
// browser. Match the shapes rather than one engine's wording.
function isChunkLoadError(error: unknown): boolean {
  const msg = error instanceof Error ? error.message : String(error);
  return (
    /Failed to fetch dynamically imported module/i.test(msg) ||
    /error loading dynamically imported module/i.test(msg) ||
    /Importing a module script failed/i.test(msg) ||
    /ChunkLoadError/i.test(msg)
  );
}

/**
 * Catches render errors from the lazy-loaded routes.
 *
 * The case this exists for: routes are code-split (issue #267), and each deploy
 * ships a fresh set of content-hashed chunks while the old ones disappear with
 * the previous image. A tab that was open across a deploy therefore 404s the
 * first time it navigates to a route it had not already fetched. Without a
 * boundary that is a blank page plus an #errors-web alert for what is really
 * just an out-of-date tab — so we reload once, which picks up the new
 * index.html and its new chunk hashes.
 *
 * Anything else is a real crash: report it (see `reportCaughtError` — a
 * boundary otherwise swallows it) and render a message.
 *
 * `resetKey` must change whenever the route does. This boundary is mounted in
 * `Layout`, ABOVE `<Outlet />`, so it outlives every navigation: without a
 * reset, one bad route would leave the error UI in place for the whole site
 * until a manual reload, including on routes that render perfectly well.
 */
export default class RouteErrorBoundary extends Component<
  { children: ReactNode; resetKey: string },
  { failed: boolean; offline: boolean }
> {
  state = { failed: false, offline: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    if (isChunkLoadError(error)) {
      // Offline is the OTHER reason a dynamic import fails, and reloading is
      // the worst available response to it. Before code splitting, in-app
      // navigation needed no network for JS, so a user on flaky wifi or in a
      // tunnel kept a working app and only data fetches failed. Reloading now
      // would throw that away: the reload is itself a network navigation, it
      // fails too, and the browser replaces the app with its own error page —
      // permanent loss of a loaded app for a transient condition. Leave the
      // guard unset and nothing reported: this is the user's network, not a
      // deploy, and it will be a stale tab again next time.
      if (navigator.onLine === false) {
        this.setState({ offline: true });
        return;
      }
      const key = reloadKey(window.location.pathname);
      let alreadyReloaded = false;
      try {
        alreadyReloaded = sessionStorage.getItem(key) === '1';
        sessionStorage.setItem(key, '1');
      } catch {
        // Private mode / storage disabled — fall through to the message rather
        // than risk an unguarded reload loop.
        alreadyReloaded = true;
      }
      if (!alreadyReloaded) {
        window.location.reload();
        return;
      }
      // A second failure on the same chunk is a missing asset, not a stale tab.
      // Worth reporting: it means a deploy shipped an index.html referencing a
      // chunk that isn't being served.
    }
    reportCaughtError(error, info.componentStack ?? undefined);
    console.error('Route render failed', error, info.componentStack);
  }

  componentDidUpdate(prev: { resetKey: string }, prevState: { failed: boolean }) {
    // Navigating away from the route that threw clears the error.
    //
    // Gated on the PREVIOUS state, not the current one. React runs
    // `componentDidUpdate` before `componentDidCatch` within a single commit,
    // and `prev` there is the last COMMITTED props — so when a route throws
    // synchronously during the navigation render, the new resetKey and the
    // error arrive in the same commit and a current-state check would reset
    // `failed` right back to false, re-render the crashing subtree and catch a
    // second time. That is reachable: `React.lazy` caches a rejected import,
    // so returning to a route whose chunk already failed throws with no
    // intervening fallback commit. The double catch made a plain stale-tab
    // reload also fire an #errors-web report, since the second pass saw the
    // guard the first had just set. `prevState.failed` is false in the error
    // commit and true only once the fallback has actually been on screen,
    // which is precisely when a navigation should clear it.
    if (prevState.failed && prev.resetKey !== this.props.resetKey) {
      this.setState({ failed: false, offline: false });
    }
  }

  componentDidMount() {
    window.addEventListener('online', this.handleOnline);
  }

  componentWillUnmount() {
    window.removeEventListener('online', this.handleOnline);
  }

  // Reconnecting clears the offline message on its own. Without this the only
  // escape is a route change: the offline branch renders no Reload button (by
  // design), and clicking the SAME nav link again produces an identical
  // resetKey, so the reset above never fires and the message would sit there
  // after the network came back.
  private handleOnline = () => {
    if (this.state.offline) this.setState({ failed: false, offline: false });
  };

  render() {
    if (this.state.failed) {
      // Offline gets its own message and NO reload button. Offering one would
      // hand the user the exact action `componentDidCatch` refuses to take on
      // their behalf: a reload with no network replaces the still-working app
      // with the browser's error page. Routes already in memory still work, so
      // say so and leave the nav as the way out.
      if (this.state.offline) {
        return (
          <div className="text-gray-400">
            This page could not load because you appear to be offline. Pages you
            have already opened still work.
          </div>
        );
      }
      return (
        <div className="text-gray-400">
          Something went wrong loading this page.{' '}
          <button
            type="button"
            className="text-blue-400 underline"
            onClick={() => window.location.reload()}
          >
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

/**
 * Disarms the one-shot reload guard once a route chunk has actually loaded.
 *
 * Render this INSIDE the `<Suspense>` boundary, as a sibling of the routed
 * element: it mounts only after the pending lazy import resolves, which is the
 * signal that the reload worked. Clearing the flag any earlier would re-arm the
 * guard before the failing chunk had been retried, turning a stale tab into a
 * reload loop; never clearing it would leave the tab unable to recover from a
 * second deploy in the same session.
 */
export function ChunkReloadReset() {
  useEffect(() => {
    try {
      // Only THIS route's guard — clearing every route's would undo the
      // per-route keying above and let a known-missing chunk reload again.
      sessionStorage.removeItem(reloadKey(window.location.pathname));
    } catch {
      // Storage unavailable — nothing to clear.
    }
  }, []);
  return null;
}
