import { Component, useEffect, type ErrorInfo, type ReactNode } from 'react';

// One-shot reload guard. Keyed in sessionStorage so a chunk that is genuinely
// missing (rather than merely stale) can't put the tab in a reload loop.
const RELOAD_KEY = 'cstat:chunk-reload';

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
 * Anything that is not a chunk-load failure is a real crash: render a plain
 * message and let React's default handling report it.
 */
export default class RouteErrorBoundary extends Component<
  { children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    if (isChunkLoadError(error)) {
      let alreadyReloaded = false;
      try {
        alreadyReloaded = sessionStorage.getItem(RELOAD_KEY) === '1';
        sessionStorage.setItem(RELOAD_KEY, '1');
      } catch {
        // Private mode / storage disabled — fall through to the message rather
        // than risk an unguarded reload loop.
        alreadyReloaded = true;
      }
      if (!alreadyReloaded) {
        window.location.reload();
        return;
      }
    }
    console.error('Route render failed', error, info.componentStack);
  }

  render() {
    if (this.state.failed) {
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
      sessionStorage.removeItem(RELOAD_KEY);
    } catch {
      // Storage unavailable — nothing to clear.
    }
  }, []);
  return null;
}
