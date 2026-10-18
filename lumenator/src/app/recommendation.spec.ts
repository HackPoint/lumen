import { isRecommendation, recommendations } from './recommendation';
import type { ContextReport, FileHotspot } from './components/index';

/**
 * The rule that puts Why in the navigation. Each boundary is pinned on both sides, because a
 * threshold one off either way changes which installs see the screen.
 */
describe('recommendation rule', () => {
  const ADVICE = 'advice the backend wrote';

  function file(over: Partial<FileHotspot> = {}): FileHotspot {
    return {
      path: '/p/a.ts',
      name: 'a.ts',
      reads: 1,
      totalTokens: 100,
      sharePct: 1,
      lines: 50,
      unchangedRereads: 0,
      recommendation: ADVICE,
      ...over,
    };
  }

  it('picks a large file read many times: at least 1,000 lines and 20 reads', () => {
    expect(isRecommendation(file({ lines: 1_000, reads: 20 }))).toBe(true);
    expect(isRecommendation(file({ lines: 999, reads: 20 }))).toBe(false);
    expect(isRecommendation(file({ lines: 1_000, reads: 19 }))).toBe(false);
    expect(isRecommendation(file({ lines: null, reads: 500 }))).toBe(false);
  });

  it('picks a file read at least 10 times, at least half of them unchanged', () => {
    expect(isRecommendation(file({ reads: 10, unchangedRereads: 5 }))).toBe(true);
    expect(isRecommendation(file({ reads: 10, unchangedRereads: 4 }))).toBe(false);
    expect(isRecommendation(file({ reads: 9, unchangedRereads: 9 }))).toBe(false);
  });

  it('does not pick a file only for its share of everything read', () => {
    expect(isRecommendation(file({ sharePct: 95, reads: 2 }))).toBe(false);
  });

  it('never picks a file the backend wrote no advice for, whatever its figures', () => {
    expect(isRecommendation(file({ lines: 5_000, reads: 100, recommendation: null }))).toBe(false);
  });

  it('lists the files to act on in the order the report ranks them, and none without a report', () => {
    const report: ContextReport = {
      totalTokensRead: 1_000,
      distinctFiles: 3,
      top10SharePct: 100,
      totalUnchangedRereads: 5,
      topFiles: [
        file({ path: '/heavy', lines: 2_000, reads: 30 }),
        file({ path: '/plain' }),
        file({ path: '/again', reads: 10, unchangedRereads: 5 }),
      ],
    };
    expect(recommendations(report).map((f) => f.path)).toEqual(['/heavy', '/again']);
    expect(recommendations(null)).toEqual([]);
  });
});
