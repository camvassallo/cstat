# Domain: camalytics.org, with campom.org kept open

**Status:** code side done (this doc ships with it); DNS, Railway and Search Console steps are manual and listed below.

The site rebranded from CamPom to Camalytics (#277) but still served from `campom.org`. It now presents as **`camalytics.org`**, and **`campom.org` stays open indefinitely** — it is not redirected away.

That second half is a deliberate departure from the original plan in #278, which called for a permanent 301. Both hosts serve the same application from the same Railway service. Nobody's bookmark breaks, and a link shared under the old name still lands on a working page under the old name.

## The problem that creates, and the fix

Two hostnames serving byte-identical HTML is textbook duplicate content. Without a redirect, nothing tells a crawler that `campom.org/players` and `camalytics.org/players` are one page rather than two competing copies, and Google picks a winner on its own — possibly the one we didn't want, and possibly a different one per URL.

The remedy, and the only one available once a 301 is off the table, is a **cross-domain `rel="canonical"`**: every page, on every host, names its `camalytics.org` URL as the canonical one. Google treats that as an instruction to consolidate ranking signals onto the named URL. Link equity built on the old domain flows to the new one; the old URLs keep resolving for humans and simply drop out of the index over time.

So the canonical tag is not decoration here. It is the entire migration mechanism, which is why the code treats a page without one as a bug.

### How it is enforced

- `routes::sitemap::CANONICAL_ORIGIN` is the single source of truth, overridable per-environment with `PUBLIC_BASE_URL`. Every absolute URL the server emits — sitemap `<loc>`, `rel="canonical"`, `og:url`, `og:image` — is built from it, never from the request's `Host`. A request arriving on `campom.org` is answered with `camalytics.org` URLs, which is exactly the point.
- **Every** HTML document route emits a canonical. Entity routes (`/players/:id`, `/teams/:id`, `/coaches/:id`) always did; `routes::meta::spa_document` now covers everything else (`/`, `/players`, `/predict`, `/portle`, unknown paths) by re-emitting `index.html`'s default head with a path-correct `rel="canonical"` + `og:url` swapped in.
- That handler replaced the plain `ServeFile` fallback under `ServeDir`. `append_index_html_on_directories(false)` is load-bearing: left at its default, `ServeDir` answers `/` straight out of `index.html`, and the homepage — the single URL most worth consolidating — would have been the only page without a canonical.
- The canonical drops the query string and any trailing slash, so `/predict`, `/predict/` and `/predict?season=2025` collapse onto one indexable URL matching what the sitemap lists.
- `crates/cstat-api/tests/canonical_host.rs` fails the build if any Rust/TS/HTML/robots source hardcodes the legacy host in URL or string-literal position. Prose mentions are fine — the arrangement needs explaining — and a deliberate literal can opt out with an `ALLOW_LEGACY_HOST` marker on the line.

### What is deliberately *not* done

- **No 301.** The old host serves 200s. This is the user-facing choice the whole design follows from.
- **No `Host`-aware URL generation.** Tempting, but self-defeating: serving `campom.org` canonicals to requests on `campom.org` is precisely the duplicate-content split the canonical exists to prevent.
- **No `noindex` on the old host.** `noindex` would remove those URLs from the index *without* passing their equity along. The canonical consolidates; `noindex` discards.

## Manual steps

### Railway
1. Add `camalytics.org` and `www.camalytics.org` as custom domains on the API service. Railway issues the Let's Encrypt cert once DNS resolves.
2. Keep `campom.org` / `www.campom.org` attached to the same service. Removing them is what would break the old links.
3. Set `PUBLIC_BASE_URL=https://camalytics.org` on the API service. Do this **before** DNS flips, so the first crawler to arrive already sees correct canonicals. (The code default is the same value, so this is belt-and-braces — but an explicit var is what makes a future move a config change rather than a deploy.)

### Cloudflare
4. In the `camalytics.org` zone, CNAME the apex and `www` at the Railway target, proxied — mirroring how `campom.org` is set up (ROADMAP.md:302).
5. **Add no redirect rule on the `campom.org` zone.** It stays a live origin.
6. Purge the cache on **both** zones after cutover.
7. `CF_ZONE_ID` on the nightly cron service names the one zone whose cache gets purged after a successful compute (`docs/deploy_nightly_cron.md`). Point it at the **`camalytics.org`** zone — that is where real traffic will land. The `campom.org` zone then relies on the 5-minute `Cache-Control` TTL, which is the correct trade for a legacy host: stale-by-minutes on a domain nobody is actively browsing is not worth a second purge call in the nightly's critical path.

### Registration
8. **Renew `campom.org` indefinitely.** Every step above depends on it resolving. Letting it lapse throws away the index equity *and* breaks every link already in the wild — and, because the Change-of-Address alternative is unavailable to us (it requires the 301 we chose not to do), there is no recovery path.

### Search Console
9. Add and verify `camalytics.org` as a new property, and submit `https://camalytics.org/sitemap.xml`.
10. **Keep the `campom.org` property verified.** `robots.txt` is served identically from both hosts and names the canonical sitemap, so fetched from the old host it is a cross-domain sitemap reference — which Search Console honours only while both properties are verified.
11. **Do not use Change of Address.** It requires a 301 from the old property. Ours is the canonical-based migration instead; Google handles it, just more slowly.
12. Expect a few weeks of turbulence. Watch `camalytics.org` coverage climb as `campom.org` URLs are reported "Duplicate, Google chose a different canonical" — that message is the migration working, not a fault.

## Verification

```bash
# Both hosts serve — the old one 200s, it does not redirect.
curl -sI https://campom.org/players | head -1
curl -sI https://camalytics.org/players | head -1

# Both name the SAME canonical, on the new host.
curl -s https://campom.org/players    | grep -o '<link rel="canonical"[^>]*>'
curl -s https://camalytics.org/players | grep -o '<link rel="canonical"[^>]*>'

# Sitemap is absolute on the canonical host from either origin.
curl -s https://campom.org/sitemap.xml | head -4

# Health responds on the new host.
curl -s https://camalytics.org/api/health
curl -s https://camalytics.org/api/health/ingest
```

Then re-scrape the social debuggers (X card validator, Facebook sharing debugger, a Discord/Slack paste) against `camalytics.org` URLs.

## Out of scope

Renaming the GitHub repo or the `cstat` crate/binary names — internal identifiers with no user impact.
