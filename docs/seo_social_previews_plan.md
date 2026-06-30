# SEO + social-preview plan (per-page unfurls + sitemap)

*Scoping doc — drafted 2026-06-30. Status: **#1, #2a, #2b, and #3 all shipped 2026-06-30.** Only JSON-LD structured data remains proposed. See "Shipped" note at the bottom.*

## Background — why the SPA needs server help

The frontend is a client-rendered React SPA: the Rust API serves the same
`web/dist/index.html` for every non-API route (`crates/cstat-api/src/main.rs`,
`ServeDir::new(spa_dir).fallback(ServeFile::new(index.html))`), and React fills
in `<div id="root">` on the client. `usePageTitle()` sets `document.title`
per route **client-side**.

Googlebot renders JS, so it sees the per-route titles. But **social-link
unfurlers (X/Twitterbot, Discordbot, Slackbot, facebookexternalhit, iMessage) do
NOT run JS** — they read the raw served HTML `<head>` only. So today every shared
link falls back to the static defaults in `index.html`.

- **#1 (shipped):** good *default* OG/Twitter/description tags in `index.html`, so
  any shared link unfurls as a branded card instead of a blank "CamPom". Needs a
  real raster image at `web/public/og-image.png` (1200x630 PNG; SVG won't unfurl).
- **#2 (below):** *per-page* meta, injected server-side, so a specific player/team
  link unfurls with that entity's name + stats (+ eventually a generated card image).
- **#3 (below):** a `sitemap.xml` so Google discovers every player/team/coach page,
  not just the ones reachable by following links.

---

## #2 — Per-page meta injection (server-side)

**Goal:** a link to `/players/:id` or `/teams/:id` (or `/coaches/:id`) unfurls with
that entity's `<title>` + `og:*`/`twitter:*` (name, team, headline metric), because
those tags are present in the *served* HTML, not added by JS afterward.

### Approach
Intercept the **document** requests for entity routes in Axum (before the SPA
`ServeDir` fallback), look the entity up, and splice per-page tags into
`index.html` before serving. Everything else falls through to `ServeDir` unchanged.
`/api/*` is untouched.

### Mechanics
1. **Mark an injection region in `index.html`.** Wrap the *overridable* tags (the
   `<title>`, `description`, `canonical`, `og:title`/`og:description`/`og:image`,
   `twitter:*`) between markers, leaving the truly-static tags (`og:type`,
   `og:site_name`, charset, viewport) outside:
   ```html
   <!--SSR_META_START--> …default title+og+twitter… <!--SSR_META_END-->
   ```
   Non-entity routes serve the file as-is (defaults apply). Entity routes replace
   the whole region — so there are **no duplicate OG tags** (duplicates are
   ambiguous to unfurlers and must be avoided).
2. **Load `index.html` once** into `AppState` at startup (it's already on disk;
   avoids a per-request file read).
3. **Add document routes** in `main.rs` for `/players/:id`, `/teams/:id`,
   `/coaches/:id`, mounted **before** the SPA fallback and **outside** the
   data-route guards. Each handler: resolve `?season=` like the API detail routes
   already do (`resolve_{player,team}_id_for_season`), fetch a small meta struct,
   render the region, return `text/html`.
4. **Lightweight meta queries** (new small fns in `queries.rs`, or reuse the detail
   queries trimmed to the needed columns):
   - Player: name, team name, season, headline metric (`cam_gbpm_v3` + percentile),
     archetype, position/class → title `"Cooper Flagg — Duke (2026) | CamPom"`,
     description `"Forward · <archetype> · +6.2 CamPom (92nd pct)"`.
   - Team: name, season, AdjEM + rank, W-L → `"Duke Blue Devils 2026 | CamPom"`,
     `"AdjEM +28.4 (3rd) · 28-4"`.
   - Coach: name, current team, career CAE.
5. **Caching.** Set a short `Cache-Control` (e.g. 5 min, matching the data routes)
   so Cloudflare serves repeat unfurls from the edge. Add `rel="canonical"` to the
   clean current-season URL so `?season=` variants don't read as duplicate content.
6. **Serve to everyone, not just bots.** UA-sniffing for unfurler agents
   (`Twitterbot`, `Discordbot`, `Slackbot`, `facebookexternalhit`) is an optional
   optimization to skip the DB hit for real users, but the lookup is one cheap
   indexed query + edge-cached, so injecting for all document requests is simpler
   and correct (React still mounts and takes over normally).

### Phase 2b (stretch) — generated OG image
Phase 2a reuses the static `og-image.png` for all entity pages (title+description
per page is already the big win). The premium version is a per-entity **stat-card
PNG**: an Axum route `GET /og/players/:id.png` that templates an SVG (name +
CamPom rating + archetype + sparkline) and rasterizes it (`resvg`/`usvg` crate),
cached immutably per `(id, season)`. Point `og:image` at it. Higher effort
(image pipeline + fonts in the runtime image); do after 2a proves out.

### Effort / risk
Medium. Self-contained (new routes + small queries + an `index.html` marker). No
schema, no model changes. Main risks: duplicate-OG-tag bugs (solved by the marker
region) and edge-cache correctness (set explicit `Cache-Control`). Backtest by
pasting links into X's post composer / Discord and a local `curl -A Twitterbot`.

---

## #3 — `sitemap.xml`

**Goal:** let Google index every player/team/coach page directly instead of only
those reachable by crawling links. High value for a DB-driven site with thousands
of entity pages.

### Approach
A Rust route (or a file generated by the nightly) that emits the URL set from the
DB. Sitemaps cap at **50,000 URLs / 50 MB** each, and players × seasons will exceed
that, so use a **sitemap index**:

- `GET /sitemap.xml` → index referencing child sitemaps.
- `GET /sitemap-players-{season}.xml`, `/sitemap-teams-{season}.xml`,
  `/sitemap-coaches.xml`, `/sitemap-static.xml`.

### Mechanics
1. **URL set:**
   - Static: `/`, `/predict`, `/transfers`, `/coaches`, `/lineups`, …
   - `/players/:id`, `/teams/:id`, `/coaches/:id` — emit the **current season's**
     canonical UUID per entity to avoid duplicate-content bloat; rely on the
     `rel="canonical"` from #2 for cross-season variants. (Optionally include prior
     seasons later, each with its own `<lastmod>`.)
2. **Queries:** reuse the `/api/seasons` season list; `SELECT id, … FROM players/
   teams/coaches WHERE season = $newest`. Add `<lastmod>` from the latest
   `games`/compute date so Google re-crawls changed pages.
3. **Serving:** `Content-Type: application/xml`; cache with a ~1h TTL, or generate
   to static files in the `nightly` run and serve them (cheaper, always fresh after
   the nightly). Either is fine; the route-with-TTL is less plumbing.
4. **Reference it in `robots.txt`:** add a line to `web/public/robots.txt`:
   ```
   Sitemap: https://campom.org/sitemap.xml
   ```
5. **Submit** the sitemap in Google Search Console (also the place to confirm
   indexing + see crawl stats).

### Effort / risk
Low–medium. One module of read-only queries + XML emission, plus a `robots.txt`
line. No schema or model changes.

---

## Suggested order

1. **#1 (done)** — static defaults. Add `og-image.png` to ship it fully.
2. **#3** — cheapest, broad indexing win; unblocks Search Console.
3. **#2a** — per-page text meta (the X-share win), reusing the static image.
4. **#2b** — generated per-entity OG card image (stretch).

Plus the smaller items from the SEO review: `rel="canonical"` everywhere (folded
into #2), and JSON-LD structured data (`Person` for players, `SportsTeam` for
teams) for rich-result eligibility — a later, low-priority add.

---

## Shipped 2026-06-30 (#1, #2a, #3)

- **#1** — static default OG/Twitter/description meta in `web/index.html`, with
  the overridable tags wrapped in `<!--SSR_META_START/END-->` markers. Image:
  `web/public/og-image.png` (1424x752, ~1.91:1).
- **#2a** — `crates/cstat-api/src/routes/meta.rs`: `SpaTemplate` parses
  `index.html` once at startup (held on `AppState.spa_index`); the document
  routes `/players/{id}`, `/teams/{id}`, `/coaches/{id}` (mounted in `main.rs`
  before the SPA fallback) look the entity up and inject per-page title +
  canonical + OG/Twitter into the marker region. Non-UUID segments (e.g.
  `/players/compare`), misses, and DB errors fall back to the default page.
  Dynamic values are HTML-escaped. Live-verified: "Darnez Slater — Colorado St.
  (2026)", "Rider 2026 · AdjEM -29.7 (#356) · 4-24 · MAAC", coach CAE line.
  Player pages point `og:image` at the #2b generated card; teams/coaches reuse
  the static `og-image.png`.
- **#2b** — `crates/cstat-api/src/routes/og_image.rs`: `GET /og/players/{id}.png`
  renders a 1200x630 PNG stat card (wordmark, name, team·season, big CamPom
  value, archetype/position/class) from an SVG template via `resvg`. Fonts: a
  vendored DejaVu face (`crates/cstat-api/assets/fonts/`) embedded with
  `include_bytes!` + loaded once into a shared `fontdb` — so it renders in the
  slim Docker image with no system fonts. Fail-soft: non-UUID / missing / render
  error → 307 redirect to the static `/og-image.png`. Edge-cached 6h. Brand:
  bg `#0b0b0c`, accent `#60a5fa` (the navbar wordmark blue). Live-verified
  (75 KB, exactly 1200x630, visually clean). **The `.ttf` files must be
  committed** — they're compiled into the binary, so the Docker build fails
  without them.
- **#3** — `crates/cstat-api/src/routes/sitemap.rs`: `/sitemap.xml` index +
  `/sitemap-{static,teams,players,coaches}.xml`, DB-generated, newest-season
  scoped (players 5096, teams 364, coaches 687 — all under the 50k cap), clean
  canonical URLs, `application/xml`, 1h cache. Referenced in
  `web/public/robots.txt` via `Sitemap:`. Submit it in Google Search Console.
- **Config:** `PUBLIC_BASE_URL` (default `https://campom.org`) feeds the absolute
  URLs in both the sitemap and the injected `og:url`/canonical.
