import { Component, useEffect, type ErrorInfo, type ReactNode } from 'react';
import { reportCaughtError, routePattern } from '../lib/errorReporter';

// One-shot reload guard, in sessionStorage so a chunk that is genuinely missing
// (rather than merely stale) can't put the tab in a reload loop.
//
// Keyed PER ROUTE. A single global flag was not actually one-shot: `React.lazy`
// caches a rejected import forever, so once a chunk is known-missing, visiting
// any working route in between cleared the flag and the next visit to the
// broken one spent another whole page reload, discarding in-page state each
// time. Per-route, a route that has already burned its reload stays burned —
// including across a later deploy that repairs the chunk, since the retry that
// would fetch the repair is the very thing being suppressed. The Reload button
// on the error UI is the way out of that, and being the user's call it cannot
// loop.
const RELOAD_KEY_PREFIX = 'cstat:chunk-reload:';
const reloadKey = (pathname: string) => RELOAD_KEY_PREFIX + routePattern(pathname);

// How long to wait for the origin before treating it as unreachable. Short:
// this only runs on a path that has already failed, with the user looking at a
// spinner until it resolves.
const PROBE_TIMEOUT_MS = 5000;

// A failed `React.lazy` import surfaces as a TypeError whose message varies by
// browser. Match the shapes rather than one engine's wording.
function isChunkLoadError(error: unknown): boolean {
  const msg = error instanceof Error ? error.message : String(error);
  return (
    /Failed to fetch dynamically imported module/i.test(msg) ||
    /error loading dynamically imported module/i.test(msg) ||
    /Importing a module script failed/i.test(msg) ||
    /ChunkLoadError/i.test(msg) ||
    // A chunk URL answered with HTML rather than JavaScript. This PR stops the
    // server producing that, but an edge that cached `200 text/html` under a
    // hashed chunk URL before the fix stays poisoned for the full year the
    // `immutable` header bought, so it outlives this deploy. Wording differs
    // per engine; these are Chrome, Firefox and Safari.
    /Expected a JavaScript module script/i.test(msg) ||
    /disallowed MIME type/i.test(msg) ||
    /not a valid JavaScript MIME type/i.test(msg) ||
    // Vite's preload helper rejects with this when a chunk's own stylesheet
    // fails. Latent today — Tailwind emits one eagerly-linked index-*.css and
    // no route chunk carries CSS — but the moment a lazy route imports a
    // stylesheet, Vite splits one out and a stale tab hits this instead. Left
    // unmatched it would skip the reload and report on the FIRST failure.
    /Unable to preload CSS/i.test(msg)
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
  { failed: boolean; offline: boolean; checking: boolean; wasChunkError: boolean }
> {
  state = { failed: false, offline: false, checking: false, wasChunkError: false };

  // Bumped whenever the boundary resets or unmounts, so an in-flight
  // reachability probe can tell that its attempt has been superseded.
  private probeGeneration = 0;

  // True while a probe is outstanding. An instance field rather than state
  // because it is read synchronously inside `componentDidCatch`, before any
  // queued setState has been applied.
  private probeInFlight = false;

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    if (isChunkLoadError(error)) {
      // A reset and a throw can land in the SAME commit — `componentDidUpdate`
      // runs first, clears `failed`, and the re-render throws again — so this
      // can be entered twice for one navigation. That is the true->true case
      // the `prevState` gate below does not cover. Without this, one navigation
      // starts two probes and two reloads.
      if (this.probeInFlight) return;
      // A dead network is the OTHER reason a dynamic import fails, and
      // reloading is the worst available response to it. Before code
      // splitting, in-app navigation needed no network for JS, so a user on
      // flaky wifi or in a tunnel kept a working app and only data fetches
      // failed. Reloading throws that away: the reload is itself a network
      // navigation, it fails too, and the browser replaces the app with its
      // own error page — permanent loss of a loaded app for a transient
      // condition.
      //
      // `navigator.onLine === false` is a fast path, not the test. It only
      // reports whether an interface exists, so it is TRUE on a captive
      // portal, a dead uplink, hotel wifi, and whenever the origin alone is
      // unreachable — which is most of what "offline" means in practice. So
      // when it claims we are online we confirm by actually reaching the
      // origin before discarding the app; see `reloadIfOriginReachable`.
      //
      // Note the asymmetry, which is why this is the ONLY place that sets
      // `offline`. A false here is trustworthy — the OS sees no interface — so
      // the message is accurate, suppressing the Reload button is right, and
      // the `online` event is guaranteed to fire and clear it. A true tells us
      // nothing, so nothing downstream may claim to know the user is offline.
      if (navigator.onLine === false) {
        this.setState({ offline: true, wasChunkError: true });
        return;
      }
      const key = reloadKey(window.location.pathname);
      let alreadyReloaded = false;
      let storageUsable = true;
      try {
        alreadyReloaded = sessionStorage.getItem(key) === '1';
      } catch {
        // Site data blocked (Chrome/Firefox "block all cookies", some in-app
        // WebViews). We cannot record a reload, so we must not start one — an
        // unrecordable reload repeats on the next document and becomes a loop.
        storageUsable = false;
      }
      if (storageUsable && !alreadyReloaded) {
        this.setState({ checking: true, wasChunkError: true });
        this.probeInFlight = true;
        this.reloadIfOriginReachable(key);
        return;
      }
      if (!storageUsable) {
        this.setState({ wasChunkError: true });
        // Show the error UI, but say nothing to #errors-web. Unreadable storage
        // is not evidence about the chunk: this is most likely an ordinary
        // stale tab, which is the case that is supposed to report nothing, and
        // alerting here would fire once per such user per deploy — precisely
        // the false alarm the guard exists to avoid.
        return;
      }
      // A second failure on the same chunk is a missing asset, not a stale tab.
      // Worth reporting: it means a deploy shipped an index.html referencing a
      // chunk that isn't being served.
      this.setState({ wasChunkError: true });
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
      this.probeGeneration += 1;
      this.probeInFlight = false;
      this.setState({ failed: false, offline: false, checking: false, wasChunkError: false });
    }
  }

  /**
   * Reload only once the origin actually answers.
   *
   * The reload exists to recover a tab left stale by a deploy, and that is only
   * worth doing if there is a server to reload from. A HEAD of the shell is one
   * round trip on a path that is already an error, and it separates the two
   * cases `navigator.onLine` cannot: a real deploy (origin answers, reload
   * recovers) from a dead connection (nothing answers, reloading would destroy
   * a working app). `redirected` catches the captive portal that returns 200
   * for someone else's page; over HTTPS most portals fail the fetch outright.
   *
   * The guard is written only on the branch that actually reloads, so a failed
   * probe does not spend this route's single retry.
   */
  private reloadIfOriginReachable(key: string) {
    // Everything below is scoped to one attempt. The boundary can be reset out
    // from under an in-flight probe — the user clicks a nav link, the reset
    // fires, and a route whose chunk is already loaded renders fine — at which
    // point a late `then` would hard-reload a working page AND spend the
    // failed route's one-shot guard for an attempt that never happened,
    // sending its next visit straight to the error UI. `generation` makes a
    // superseded probe a no-op.
    const generation = this.probeGeneration;
    const superseded = () => generation !== this.probeGeneration;

    // A black-holed network never rejects, so without a deadline `checking`
    // would render "Loading…" until the browser's own fetch timeout.
    const controller = new AbortController();
    const timer = window.setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS);

    // The whole call is wrapped, not just the promise: this runs while React is
    // already handling an error, and a throw here would escalate past this
    // boundary and unmount the app. A probe that cannot even start is treated
    // the same as one that fails.
    try {
      fetch('/index.html', {
        method: 'HEAD',
        cache: 'no-store',
        signal: controller.signal,
      })
        .then((res) => {
          window.clearTimeout(timer);
          if (superseded()) return;
          this.probeInFlight = false;
          // An origin that ANSWERS is reachable, whatever it answered. A 502 or
          // 503 from the proxy mid-rollout, or a redirect from an edge rule on
          // the second host, means the network is fine — and the user must not
          // be told they are offline, because that message deliberately has no
          // Reload button, `navigator.onLine` was true throughout so the
          // `online` event never fires, and re-clicking the same nav link keeps
          // the same resetKey. It would be a dead end. Fall through to the
          // ordinary error UI, where Reload is offered and is the right action
          // once the rollout settles.
          if (!res.ok || res.redirected) {
            this.setState({ checking: false });
            return;
          }
          // The guard write gets its own try, not the probe's catch: the two
          // failures are different things. An unwritable guard (Safari private
          // mode, quota) says nothing about the network, so reporting it as
          // "offline" would be a lie — and we cannot just reload anyway, since
          // a reload we are unable to record repeats on the next document and
          // becomes the loop the guard exists to prevent. Fall through to the
          // ordinary error UI instead, where the reload is the user's call and
          // therefore bounded.
          try {
            sessionStorage.setItem(key, '1');
          } catch {
            this.setState({ checking: false });
            return;
          }
          window.location.reload();
        })
        .catch(() => {
          // The fetch failed or timed out. Tempting to call that offline, but
          // this path is only reachable with `navigator.onLine === true`, so
          // the offline UI would be a trap: it renders no Reload button on the
          // grounds that `handleOnline` will clear it, and `online` fires only
          // on a false->true transition that cannot happen here. The user would
          // be stuck on a wrong diagnosis with no way out but a different
          // route. A DNS blip or a proxy hiccup also resolves in seconds, so
          // the ordinary error UI — vague but honest, and with a Reload button
          // that is now the right move — is the better answer.
          window.clearTimeout(timer);
          if (superseded()) return;
          this.probeInFlight = false;
          this.setState({ checking: false });
        });
    } catch {
      // Same reasoning as the catch above: reachable only while `onLine` is
      // true, so it must not claim the user is offline.
      window.clearTimeout(timer);
      this.probeInFlight = false;
      this.setState({ checking: false });
    }
  }

  componentDidMount() {
    window.addEventListener('online', this.handleOnline);
  }

  componentWillUnmount() {
    this.probeGeneration += 1;
    this.probeInFlight = false;
    window.removeEventListener('online', this.handleOnline);
  }

  // Reconnecting retries on its own. Without this the only escape is a route
  // change: the offline branch renders no Reload button (by design), and
  // clicking the SAME nav link again produces an identical resetKey, so the
  // reset above never fires and the message would sit there after the network
  // came back.
  //
  // Note what the retry actually is. `React.lazy` stores a rejection
  // permanently, so clearing the state re-renders the same rejected element,
  // it throws again, and recovery goes through the reload path above — a full
  // page load, not an in-place re-import. That is the only thing that can work,
  // and it is now gated on the origin answering, which matters most here: the
  // `online` event fires exactly when a flapping connection is likeliest to
  // drop again.
  private handleOnline = () => {
    if (this.state.offline) {
      this.setState({ failed: false, offline: false, checking: false, wasChunkError: false });
    }
  };

  render() {
    if (this.state.failed) {
      // Offline gets its own message and NO reload button. Offering one would
      // hand the user the exact action `componentDidCatch` refuses to take on
      // their behalf: a reload with no network replaces the still-working app
      // with the browser's error page. Routes already in memory still work, so
      // say so and leave the nav as the way out.
      // Probe in flight: say nothing actionable yet. Rendering the Reload
      // button here would offer the very action we are still deciding is safe.
      if (this.state.checking) {
        return <div className="text-gray-400">Loading…</div>;
      }
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
            // Re-checked at CLICK time, not at catch time: the connection can
            // drop during the up-to-5s probe window, and this button is exactly
            // what `componentDidCatch` refuses to do on the user's behalf while
            // offline — the reload is a network navigation, it fails, and the
            // browser replaces a still-working app with its error page.
            //
            // Gated on `wasChunkError`, mirroring the gate `componentDidCatch`
            // applies. Being offline explains a chunk that would not load; it
            // explains nothing about a render crash, and swapping in the
            // offline message there would be both a wrong diagnosis and a trap,
            // since that message renders no button.
            onClick={() => {
              if (navigator.onLine === false && this.state.wasChunkError) {
                this.setState({ offline: true });
                return;
              }
              window.location.reload();
            }}
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
 * reload loop.
 *
 * Note the limit. This only ever clears the guard of a route whose chunk
 * RESOLVED, so the one route that burned its reload keeps its guard for the
 * rest of the tab session: `React.lazy` holds the rejection, so that chunk
 * never resolves here to clear it. A later deploy repairing that chunk needs
 * the Reload button rather than an automatic retry — recovering it
 * automatically would mean re-probing the failed chunk on every visit, which
 * buys little over the button and is the shape of loop the guard exists to
 * prevent.
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
