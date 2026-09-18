import { describe, expect, it } from 'vitest'
import { isCrawlerUserAgent, isExtensionStack, routePattern } from './errorReporter'

describe('routePattern', () => {
  it('collapses entity ids so repeat hits on one route dedup together', () => {
    // The whole point: two different players must produce the SAME key, or one
    // broken route sends a report per entity the user happened to open.
    const a = routePattern('/players/2070f186-a468-4725-9ba4-f28310adfb97')
    const b = routePattern('/players/d9469ea9-44c5-4803-8d5a-39cf4028ea3a')
    expect(a).toBe('/players/:id')
    expect(a).toBe(b)
  })

  it('keeps distinct routes distinct', () => {
    // ...while still separating genuinely different routes, which is why the
    // boundary sets `source` at all.
    expect(routePattern('/lineups')).not.toBe(routePattern('/coaches'))
    expect(routePattern('/teams/2070f186-a468-4725-9ba4-f28310adfb97')).toBe('/teams/:id')
  })

  it('collapses only the id segment of a nested entity route', () => {
    expect(routePattern('/players/2070f186-a468-4725-9ba4-f28310adfb97/progression')).toBe(
      '/players/:id/progression',
    )
  })

  it('leaves id-free paths untouched', () => {
    expect(routePattern('/')).toBe('/')
    expect(routePattern('/players')).toBe('/players')
    expect(routePattern('/which-class')).toBe('/which-class')
  })
})

describe('isCrawlerUserAgent', () => {
  it('drops the renderers that produced every recent noise alert', () => {
    // The UAs from #339, verbatim.
    expect(
      isCrawlerUserAgent(
        'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 (compatible; meta-externalagent/1.1 (+https://developers.facebook.com/docs/sharing/webmasters/crawler))',
      ),
    ).toBe(true)
    expect(
      isCrawlerUserAgent(
        'Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm) Chrome/136.0.0.0 Safari/537.36',
      ),
    ).toBe(true)
    expect(
      isCrawlerUserAgent(
        'Mozilla/5.0 (compatible; Baiduspider-render/2.0; +http://www.baidu.com/search/spider.html)',
      ),
    ).toBe(true)
  })

  it('keeps real browsers', () => {
    expect(
      isCrawlerUserAgent(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36',
      ),
    ).toBe(false)
    expect(
      isCrawlerUserAgent('Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0'),
    ).toBe(false)
    expect(
      isCrawlerUserAgent(
        'Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1',
      ),
    ).toBe(false)
    // An empty UA is not evidence of a crawler.
    expect(isCrawlerUserAgent('')).toBe(false)
  })
})

describe('isExtensionStack', () => {
  it('drops a rejection that originates in a browser extension', () => {
    // MetaMask's inpage.js, the #339 case — arrives as `unhandledrejection`,
    // which has no filename for `isThirdPartyScript` to check.
    expect(
      isExtensionStack(
        'i: Failed to connect to MetaMask\n    at Object.connect (chrome-extension://nkbihfbeogaeaoehlefnkodbefgpgknn/scripts/inpage.js:7:84292)',
      ),
    ).toBe(true)
    expect(isExtensionStack('at foo (moz-extension://abc/content.js:1:1)')).toBe(true)
  })

  it('keeps our own frames, and an empty stack', () => {
    expect(
      isExtensionStack(
        'TypeError: x is undefined\n    at tt (https://camalytics.org/assets/index-3-rxmRr3.js:11:1124)',
      ),
    ).toBe(false)
    expect(isExtensionStack('')).toBe(false)
  })
})
