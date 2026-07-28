// Vitest setup, loaded before every spec file.
//
// The Angular unit-test builder initialises the TestBed itself, so this file
// only needs to cover the browser APIs jsdom does not implement but the
// components rely on.

/**
 * jsdom has no matchMedia. The firefly mascot queries
 * `prefers-reduced-motion` to freeze its breathing glow, so without this the
 * component throws on construction.
 *
 * Defaults to "no preference" (animations on); a spec can override it.
 */
if (!('matchMedia' in globalThis.window)) {
  Object.defineProperty(globalThis.window, 'matchMedia', {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}
