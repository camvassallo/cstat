import { describe, expect, it } from 'vitest'
import { routePattern } from './errorReporter'

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
