// Forwards uncaught browser errors to the server sink (`POST /api/client-error`,
// which relays to #errors-web). Without this a client-side crash lives and dies
// in the user's console — invisible to us.
//
// Defensive by construction: reporting must never itself throw, never block the
// page, and never flood. So it is fire-and-forget (sendBeacon / keepalive
// fetch), deduped by signature, and hard-capped per page load. The server adds
// its own global throttle on top.

const ENDPOINT = '/api/client-error'

// A single bad deploy can throw on every render; cap what one page load sends.
const MAX_REPORTS_PER_LOAD = 10

interface ClientErrorReport {
  kind: 'error' | 'unhandledrejection'
  message: string
  page: string
  source: string
  stack: string
  user_agent: string
}

let sent = 0
const seen = new Set<string>()

// Errors thrown by third-party scripts we don't ship or control — Cloudflare's
// RUM beacon (static.cloudflareinsights.com), browser extensions, injected
// analytics — are pure noise: we can't fix them and they crowd out real app
// crashes in #errors-web. A cross-origin script filename is the tell. Same-origin
// (our bundle) and empty filenames (inline/opaque) still report.
//
// Concrete case this guards: issue #173, where Cloudflare's beacon.min.js called
// Array.prototype.at() on a Chrome 79 client that predates it, and we forwarded
// the third-party crash as if it were ours.
function isThirdPartyScript(filename: string): boolean {
  if (!filename) return false
  try {
    return new URL(filename).origin !== window.location.origin
  } catch {
    return false
  }
}

function report(r: ClientErrorReport): void {
  try {
    if (sent >= MAX_REPORTS_PER_LOAD) return
    const key = `${r.kind}|${r.message}|${r.source}`
    if (seen.has(key)) return
    seen.add(key)
    sent += 1

    const body = JSON.stringify(r)
    // sendBeacon is the ideal transport — fire-and-forget and it survives a page
    // unload (an error on a link click that navigates away still gets through).
    // Fall back to keepalive fetch where sendBeacon is unavailable.
    if (typeof navigator.sendBeacon === 'function') {
      navigator.sendBeacon(ENDPOINT, new Blob([body], { type: 'application/json' }))
    } else {
      void fetch(ENDPOINT, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
        keepalive: true,
      }).catch(() => {})
    }
  } catch {
    // Reporting failures are swallowed — instrumentation must never break the app.
  }
}

/**
 * Collapses a pathname to its route shape by replacing entity ids with `:id`.
 *
 * The dedup key has to separate two different routes while still merging many
 * hits on the SAME route. A raw pathname does the first and breaks the second:
 * `/players/<uuid>` is a distinct string per player, so a deploy that breaks
 * `PlayerDetail` for everyone would send a fresh report for each player a user
 * opened — up to `MAX_REPORTS_PER_LOAD` copies of one bug, where the uncaught
 * path (keyed on a stable `filename:lineno:colno`) sent exactly one.
 *
 * Every entity route here is UUID-keyed (`/teams/:id`, `/players/:id`,
 * `/players/:id/progression`, `/coaches/:id`), so matching UUID segments is
 * enough and keeps this independent of the router.
 */
export function routePattern(pathname: string): string {
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
  return pathname
    .split('/')
    .map((seg) => (UUID.test(seg) ? ':id' : seg))
    .join('/')
}

/**
 * Report an error that a React error boundary caught.
 *
 * A boundary changes which reporting path React takes, and only one of the two
 * reaches the listeners above. An UNCAUGHT error goes through
 * `defaultOnUncaughtError` -> `reportGlobalError`, which fires the window
 * `error` event; a CAUGHT one goes through `defaultOnCaughtError`, which only
 * calls `console.error` (react-dom 19.2.4). So adding `RouteErrorBoundary`
 * would have silently taken every route crash out of #errors-web unless the
 * boundary reports it itself. Routed through the same `report` as the global
 * path, so the dedup and per-load cap still apply.
 */
export function reportCaughtError(error: unknown, componentStack?: string): void {
  const err = error instanceof Error ? error : null
  report({
    kind: 'error',
    message: err?.message || String(error) || 'Unknown error',
    // `report` dedups on `kind|message|source`. The uncaught path fills
    // `source` from the ErrorEvent's `filename:lineno:colno`, which separated
    // otherwise-identical messages; a boundary has no equivalent, and leaving
    // it empty would collapse two DIFFERENT routes failing with the same
    // common message ("Cannot read properties of undefined (reading 'map')")
    // into one report, silently dropping the second bug. The route PATTERN is
    // the discriminator a boundary does have — see `routePattern` for why the
    // raw pathname is the wrong granularity.
    source: routePattern(window.location.pathname),
    // Component stack FIRST: the server caps `stack` at 600 chars, and in a
    // production build every frame of a minified JS stack carries a full
    // hashed-chunk URL (~70-90 chars each), so 8-10 frames exhaust the budget
    // on their own. Whichever trace goes second is the one that gets cut — and
    // for a boundary-caught error the component stack is the part worth
    // keeping, since it names the route and component tree that failed, while
    // the minified JS frames say little without sourcemaps and the error text
    // is already in `message`.
    stack: [componentStack, err?.stack].filter(Boolean).join('\n'),
    page: window.location.href,
    user_agent: navigator.userAgent,
  })
}

/**
 * Install the global `error` / `unhandledrejection` listeners. Call once at
 * startup, before the app mounts. Idempotent-safe to call once.
 */
export function installErrorReporter(): void {
  if (typeof window === 'undefined') return

  window.addEventListener('error', (e: ErrorEvent) => {
    // Resource-load errors (img/script 404) don't bubble to window without a
    // capture-phase listener, so anything we see here is a real script error.
    if (isThirdPartyScript(e.filename)) return
    report({
      kind: 'error',
      message: e.message || (e.error ? String(e.error) : 'Unknown error'),
      source: e.filename ? `${e.filename}:${e.lineno}:${e.colno}` : '',
      stack: e.error?.stack ?? '',
      page: window.location.href,
      user_agent: navigator.userAgent,
    })
  })

  window.addEventListener('unhandledrejection', (e: PromiseRejectionEvent) => {
    const reason: unknown = e.reason
    const message =
      reason instanceof Error ? reason.message : reason != null ? String(reason) : 'Unhandled rejection'
    report({
      kind: 'unhandledrejection',
      message,
      source: '',
      stack: reason instanceof Error ? (reason.stack ?? '') : '',
      page: window.location.href,
      user_agent: navigator.userAgent,
    })
  })
}
