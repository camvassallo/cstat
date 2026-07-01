// Tiny typed localStorage helpers. The app had no persistence layer before
// Portle (season state lives in the URL), so this is the first place
// we keep client-only state across reloads: the daily game's in-progress
// guesses and the player's streak stats.
//
// Everything is SSR-/private-mode-safe: a missing `window`, a disabled store,
// or malformed JSON degrades to the caller's fallback rather than throwing.

export function loadJson<T>(key: string, fallback: T): T {
  if (typeof window === 'undefined') return fallback;
  try {
    const raw = window.localStorage.getItem(key);
    if (raw == null) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function saveJson<T>(key: string, value: T): void {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Quota exceeded / disabled store — the game still works this session,
    // it just won't persist. Nothing to surface to the user.
  }
}
