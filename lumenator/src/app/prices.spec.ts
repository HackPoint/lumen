import {
  aggTokens, amount, figureTag, fmtUsd, isFloor, price, priceFor, priceKey, rateFor, shortfalls,
} from './prices';
import type { ModelTokens } from './prices';
import { PRICE_TABLE } from './tauri-bridge.fake';
import keyCases from './price-key.cases.json';

/**
 * Per-model pricing. Which row an id is priced under is checked against the case list the
 * Rust side reads too, so the two resolve a model id the same way; the prices themselves come
 * from the backend.
 */
describe('prices', () => {
  function tokens(model: string | null, over: Partial<ModelTokens> = {}): ModelTokens {
    return {
      model, speed: 'standard', inferenceGeo: 'global', input: 0, output: 0, cacheRead: 0,
      cacheWrite: 0, cacheWrite1h: 0, cacheWriteUnknown: 0, ...over,
    };
  }

  // ── which row a model id is priced under ──────────────────────────────────

  it('resolves every shared case to the key the Rust side resolves it to', () => {
    expect(keyCases.cases.length).toBeGreaterThan(10);
    for (const c of keyCases.cases) {
      expect(priceKey(c.id), c.id).toBe(c.key);
    }
  });

  it("finds each model's own row", () => {
    expect(priceFor(PRICE_TABLE, 'claude-sonnet-4-6')?.input).toBe(3);
    expect(priceFor(PRICE_TABLE, 'claude-haiku-4-5-20251001')?.input).toBe(1);
    expect(priceFor(PRICE_TABLE, 'claude-opus-4-8[1m]')?.input).toBe(5);
    expect(priceFor(PRICE_TABLE, 'claude-opus-5-5')?.input).toBe(4);
  });

  it('has no price for an unknown model, an unpriced one, or a near miss', () => {
    for (const id of ['', '<synthetic>', 'gpt-4', 'claude-opus-5-7', 'claude-opus-4-8-fast', 'claude-haiku-5-5']) {
      expect(priceFor(PRICE_TABLE, id)).toBeNull();
    }
    expect(priceFor(PRICE_TABLE, null)).toBeNull();
    expect(priceFor(null, 'claude-opus-4-8')).toBeNull();
  });

  // ── speed ─────────────────────────────────────────────────────────────────

  it('prices a fast turn at the fast-mode rate, and has none for a model without one', () => {
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'fast', 'global')?.prices.output).toBe(50);
    expect(rateFor(PRICE_TABLE, 'claude-sonnet-4-6', 'fast', 'global')).toBeNull();
  });

  it('prices an unrecorded speed at the standard rate, as a floor only where fast mode exists', () => {
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', null, 'global')).toEqual({
      prices: PRICE_TABLE.models[0].prices!, speedUnknown: true, geoUnknown: false,
    });
    expect(rateFor(PRICE_TABLE, 'claude-sonnet-4-6', undefined, 'global')?.speedUnknown).toBe(false);
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'standard', 'global')?.speedUnknown).toBe(false);
  });

  it('has no price for a speed it does not know', () => {
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'turbo', 'global')).toBeNull();
  });

  // ── region ────────────────────────────────────────────────────────────────

  it("prices a US-only turn at the row's multiplier on every price, fast mode's included", () => {
    const sonnet = rateFor(PRICE_TABLE, 'claude-sonnet-4-6', 'standard', 'us')!;
    expect(sonnet.geoUnknown).toBe(false);
    expect(sonnet.prices.input).toBeCloseTo(3.3, 10);
    expect(sonnet.prices.output).toBeCloseTo(16.5, 10);
    expect(sonnet.prices.cacheWrite5m).toBeCloseTo(4.125, 10);
    expect(sonnet.prices.cacheWrite1h).toBeCloseTo(6.6, 10);
    expect(sonnet.prices.cacheRead).toBeCloseTo(0.33, 10);
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'fast', 'us')!.prices.output).toBeCloseTo(55, 10);
  });

  it('prices a global turn at the price', () => {
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'standard', 'global')).toEqual({
      prices: PRICE_TABLE.models[0].prices!, speedUnknown: false, geoUnknown: false,
    });
  });

  it('prices a region not known at the global rate, as a floor only where US-only costs more', () => {
    for (const geo of [null, undefined, 'not_available']) {
      expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'standard', geo)).toEqual({
        prices: PRICE_TABLE.models[0].prices!, speedUnknown: false, geoUnknown: true,
      });
      // Haiku 4.5 does not take inference_geo: its price is its price wherever it ran.
      expect(rateFor(PRICE_TABLE, 'claude-haiku-4-5', 'standard', geo)?.geoUnknown).toBe(false);
    }
    expect(rateFor(PRICE_TABLE, 'claude-haiku-4-5', 'standard', 'us')?.prices.output).toBe(5);
  });

  it('has no price for a region it does not know, where regions are priced', () => {
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'standard', 'eu')).toBeNull();
    expect(rateFor(PRICE_TABLE, 'claude-haiku-4-5', 'standard', 'eu')).not.toBeNull();
    const p = price([tokens('claude-opus-4-8', { inferenceGeo: 'eu', output: 1 })], PRICE_TABLE);
    expect(p.unpricedModels).toEqual(['claude-opus-4-8 in region eu']);
  });

  it('keeps an empty region or speed apart from an unrecorded one: unpriced, named as Rust names it', () => {
    // `lumen_core::rates::Spend::priced` names these "claude-opus-4-8 in region " and
    // "claude-opus-4-8 at speed ": an empty value is a value this build does not know.
    expect(rateFor(PRICE_TABLE, 'claude-opus-4-8', 'standard', '')).toBeNull();
    const p = price([
      tokens('claude-opus-4-8', { inferenceGeo: '', output: 1 }),
      tokens('claude-opus-4-8', { speed: '', output: 1 }),
    ], PRICE_TABLE);
    expect(p.hasFigure).toBe(false);
    expect(p.unpricedModels).toEqual(['claude-opus-4-8 in region ', 'claude-opus-4-8 at speed ']);
  });

  it('adds a US-only turn at 1.1 times beside a global one', () => {
    const p = price([
      tokens('claude-opus-4-8', { output: 1_000_000, cacheRead: 1_000_000 }),
      tokens('claude-opus-4-8', { inferenceGeo: 'us', output: 1_000_000, cacheRead: 1_000_000 }),
    ], PRICE_TABLE);
    expect(p.output).toBeCloseTo(25 + 27.5, 10);
    expect(p.cacheSavings).toBeCloseTo(4.5 + 4.95, 10);
    expect(figureTag(p, 'total')).toBeNull();
  });

  it('makes every figure of a turn whose region is not known a floor, and says why', () => {
    const p = price([
      tokens('claude-opus-4-8', { inferenceGeo: 'not_available', output: 10, cacheRead: 10 }),
      tokens('claude-haiku-4-5', { inferenceGeo: null, input: 10 }),
    ], PRICE_TABLE);
    expect(figureTag(p, 'output')).toEqual({
      label: 'at least',
      why: 'Some turns did not record where they ran and are priced at the global rate; '
        + 'US-only inference costs more.',
    });
    expect(isFloor(p, 'savings')).toBe(true);
    expect(isFloor(p, 'input')).toBe(false);
  });

  // ── pricing ───────────────────────────────────────────────────────────────

  it('prices each kind of token at its own rate', () => {
    const p = price([tokens('claude-opus-4-8', {
      input: 1_000_000, output: 1_000_000, cacheRead: 1_000_000, cacheWrite: 1_000_000,
    })], PRICE_TABLE);
    expect(p.input).toBeCloseTo(5, 10);
    expect(p.output).toBeCloseTo(25, 10);
    expect(p.cacheRead).toBeCloseTo(0.5, 10);
    expect(p.cacheWrite).toBeCloseTo(6.25, 10);
    expect(p.total).toBeCloseTo(36.75, 10);
    expect(p.cacheSavings).toBeCloseTo(4.5, 10);
    expect(isFloor(p, 'total')).toBe(false);
    expect(figureTag(p, 'total')).toBeNull();
  });

  it('prices a one-hour cache write at the one-hour rate and the rest at the 5-minute one', () => {
    const p = price([tokens('claude-opus-4-8', { cacheWrite: 3_000_000, cacheWrite1h: 1_000_000 })], PRICE_TABLE);
    expect(p.cacheWrite).toBeCloseTo(2 * 6.25 + 10, 10);
  });

  it('prices every model at its own rate and adds them up', () => {
    const p = price([
      tokens('claude-opus-4-8', { output: 1_000_000 }),
      tokens('claude-opus-4-8', { speed: 'fast', output: 1_000_000 }),
      tokens('claude-sonnet-4-6', { output: 1_000_000 }),
      tokens('claude-haiku-4-5-20251001', { output: 1_000_000 }),
    ], PRICE_TABLE);
    expect(p.total).toBeCloseTo(25 + 50 + 15 + 5, 10);
    expect(p.pricedTokens).toBe(4_000_000);
  });

  // ── figures that are not the whole ───────────────────────────────────────

  it('adds no dollars for a model with no price and makes the figure a floor that names it', () => {
    const p = price([
      tokens('claude-opus-4-8', { output: 1_000_000 }),
      tokens('<synthetic>', { output: 7 }),
      tokens(null, { input: 3 }),
      tokens('claude-sonnet-4-6', { speed: 'fast', output: 1 }),
    ], PRICE_TABLE);
    expect(p.total).toBeCloseTo(25, 10);
    expect(p.unpricedModels).toEqual(['<synthetic>', '', 'claude-sonnet-4-6 at speed fast']);
    expect(p.hasFigure).toBe(true);
    expect(figureTag(p, 'total')).toEqual({
      label: 'at least',
      why: 'No price for <synthetic>, turns with no model, claude-sonnet-4-6 at speed fast.',
    });
    expect(figureTag(p, 'cacheRead')).toBeNull();
  });

  it('makes a cache write of unknown duration a floor on the write and the total only', () => {
    // The fixture turn in lumen-core: $0.0599 with its 1-hour split, $0.0494 without it.
    const p = price([tokens('claude-opus-4-8', {
      input: 3_197, output: 303, cacheRead: 16_685, cacheWrite: 2_799, cacheWriteUnknown: 2_799,
    })], PRICE_TABLE);
    expect(p.total).toBeCloseTo(0.04939625, 10);
    expect(figureTag(p, 'total')?.label).toBe('at least');
    expect(figureTag(p, 'total')?.why).toContain('5-minute rate');
    expect(isFloor(p, 'cacheWrite')).toBe(true);
    expect(isFloor(p, 'output')).toBe(false);
    expect(isFloor(p, 'savings')).toBe(false);
  });

  it('counts a write as of unknown duration when the row does not say otherwise', () => {
    const p = price([{ model: 'claude-opus-4-8', speed: 'standard', input: 0, output: 0, cacheRead: 0, cacheWrite: 100 }], PRICE_TABLE);
    expect(p.durationUnknown).toBe(100);
  });

  it('makes every figure of a model with an unrecorded speed a floor', () => {
    const p = price([tokens('claude-opus-4-8', { speed: null, output: 10, cacheRead: 10 })], PRICE_TABLE);
    expect(figureTag(p, 'output')?.why).toContain('did not record their speed');
    expect(isFloor(p, 'savings')).toBe(true);
    expect(isFloor(p, 'input')).toBe(false);
  });

  it('says prices have not loaded, not that a model has no price, before the table arrives', () => {
    const p = price([tokens('claude-opus-4-8', { output: 1_000_000 })], null);
    expect(p.total).toBe(0);
    expect(p.hasFigure).toBe(false);
    expect(figureTag(p, 'total')).toEqual({
      label: 'no price',
      why: 'Prices have not loaded from Lumen yet, so nothing here is priced.',
    });
    expect(fmtUsd(p, 'total')).toBe('—');
  });

  it('has no figure when nothing could be priced, and an honest zero when there was nothing', () => {
    const none = price([tokens('mystery', { output: 5 })], PRICE_TABLE);
    expect(none.hasFigure).toBe(false);
    expect(fmtUsd(none, 'total')).toBe('—');
    expect(figureTag(none, 'total')?.label).toBe('no price');

    const empty = price([], PRICE_TABLE);
    expect(empty.hasFigure).toBe(true);
    expect(fmtUsd(empty, 'total')).toBe('$0.00');
    expect(figureTag(empty, 'total')).toBeNull();

    // A model seen with no tokens leaves nothing out.
    expect(isFloor(price([tokens('<synthetic>')], PRICE_TABLE), 'total')).toBe(false);
  });

  it('reads each figure off the right field', () => {
    const p = price([tokens('claude-opus-4-8', { input: 1_000_000, cacheRead: 1_000_000 })], PRICE_TABLE);
    expect(amount(p, 'input')).toBeCloseTo(5, 10);
    expect(amount(p, 'savings')).toBeCloseTo(4.5, 10);
    expect(fmtUsd(p, 'savings')).toBe('$4.50');
    expect(shortfalls(p, 'total')).toEqual([]);
  });

  // ── aggregates ────────────────────────────────────────────────────────────

  it("reads an aggregate's split, or leaves an unsplit one under no model", () => {
    const split = { model: 'claude-opus-4-8', turns: 1, input: 1, output: 2, cacheRead: 3, cacheWrite: 4 };
    const sums = { turns: 1, input: 1, output: 2, cacheRead: 3, cacheWrite: 4, totalTokens: 10 };
    expect(aggTokens({ ...sums, byModel: [split] })).toEqual([split]);
    expect(aggTokens(sums)).toEqual([
      { model: null, input: 1, output: 2, cacheRead: 3, cacheWrite: 4 },
    ]);
  });
});
