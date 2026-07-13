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
 * Install the global `error` / `unhandledrejection` listeners. Call once at
 * startup, before the app mounts. Idempotent-safe to call once.
 */
export function installErrorReporter(): void {
  if (typeof window === 'undefined') return

  window.addEventListener('error', (e: ErrorEvent) => {
    // Resource-load errors (img/script 404) don't bubble to window without a
    // capture-phase listener, so anything we see here is a real script error.
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
