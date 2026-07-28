import { inferWindow, modelWindow, resolveWindow } from './session.service';

/**
 * Regression cover for a real report: the panel read "396,326 / 500,000" on
 * claude-opus-5, a 1M model.
 *
 * The window logic was not at fault. The daemon's snapshot query referenced
 * turns.is_subagent, a column that never reached databases created before 1.1.0,
 * so the query failed and — because the result is matched with `if let Ok(..)` —
 * no snapshot was sent at all. The GUI therefore knew only the turns observed
 * since launch, whose peak was still inside the 500K tier. Values below are the
 * ones the daemon actually served once the migration had run.
 */
const LIVE = { model: 'claude-opus-5', fill: 483_254, peakFill: 995_521 };

describe('context window on live snapshot values', () => {
  it('resolves the model to its published 1M window', () => {
    expect(modelWindow(LIVE.model)).toBe(1_000_000);
    expect(resolveWindow(LIVE.model, LIVE.peakFill)).toBe(1_000_000);
  });

  it('renders the fill against 1M rather than the 500K tier', () => {
    const max = resolveWindow(LIVE.model, LIVE.peakFill);
    expect(`${LIVE.fill.toLocaleString()} / ${max.toLocaleString()}`).toBe(
      '483,254 / 1,000,000',
    );
    expect(Math.round((LIVE.fill / max) * 100)).toBe(48);
  });

  it('only reaches 500K when the observed peak is that low', () => {
    // Which is the state a missing snapshot left the GUI in.
    expect(inferWindow(396_326)).toBe(500_000);
    // And even then the model table wins once it is consulted with a real peak.
    expect(resolveWindow('claude-opus-5', 396_326)).toBe(1_000_000);
  });
});
