// Google Analytics 4. Loaded only when the build was given a measurement id
// (`VITE_GA_MEASUREMENT_ID`). In production that is a build-time variable the
// Dockerfile passes through as a build arg; locally Vite reads it from the
// repo-root `.env` (vite.config.ts `envDir`). With no id this is a no-op.
//
// A dev server (`npm run dev`) with an id set does load it, with `debug_mode`
// on, so hits show in GA4's DebugView for testing. Leave the id out of your
// local `.env` when you are not testing, or your own browsing is counted.
//
// Page views: the SPA navigates with history.pushState, so the initial gtag
// `config` page view would be the only one. Rather than lean on GA4's
// "page changes based on browser history events" enhanced-measurement toggle
// (it fires on pushState, before the route renders and before usePageTitle has
// set the title, so every page view carried the previous page's title), the
// automatic page view is switched off and `trackPageView` sends one per route
// change from the router, after render. That toggle must therefore be OFF in the
// GA4 data stream settings, or each navigation counts twice.

type Gtag = (...args: unknown[]) => void

declare global {
  interface Window {
    dataLayer?: unknown[]
    gtag?: Gtag
  }
}

const MEASUREMENT_ID: string | undefined = import.meta.env.VITE_GA_MEASUREMENT_ID

// G-XXXXXXXXXX. Anything else is a misconfigured build variable; better to send
// nothing than to load a script that silently drops every hit.
export const analyticsEnabled = !!MEASUREMENT_ID && /^G-[A-Z0-9]+$/.test(MEASUREMENT_ID)

let installed = false

export function installAnalytics(): void {
  if (!analyticsEnabled || installed) return
  installed = true
  try {
    window.dataLayer = window.dataLayer || []
    // gtag.js reads the `arguments` object itself, not an array — the official
    // snippet's shape is load-bearing.
    window.gtag = function gtag() {
      // eslint-disable-next-line prefer-rest-params
      window.dataLayer!.push(arguments)
    }
    window.gtag('js', new Date())
    window.gtag('config', MEASUREMENT_ID, {
      send_page_view: false,
      ...(import.meta.env.DEV ? { debug_mode: true } : {}),
    })

    const s = document.createElement('script')
    s.async = true
    s.src = `https://www.googletagmanager.com/gtag/js?id=${MEASUREMENT_ID}`
    document.head.appendChild(s)
  } catch {
    // Analytics must never break the page.
  }
}

export function trackPageView(path: string, href: string, title: string): void {
  if (!analyticsEnabled || !window.gtag) return
  try {
    window.gtag('event', 'page_view', {
      page_path: path,
      page_location: href,
      page_title: title,
    })
  } catch {
    // ignore
  }
}
