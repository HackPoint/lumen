import { notASummary } from './savings-summary';
import { dollars, modelled, recordedPeriod, savingsSummary } from './savings-summary.fake';

/** What the window refuses to show as a summary: another schema, no periods, a figure that is no number. */
describe('notASummary', () => {
    it('takes a summary of its schema with finite figures', () => {
        expect(notASummary(savingsSummary({ week: recordedPeriod(10, 1.5, { modelled: modelled(0.2) }) }))).toBeNull();
    });

    it('refuses what is not one, another schema, or one with no periods', () => {
        expect(notASummary(null)).toBe('the app answered with something that is not a savings summary');
        expect(notASummary('x')).toBe('the app answered with something that is not a savings summary');
        expect(notASummary({ ...savingsSummary(), schema: 2 })).toBe(
            'the app sent a savings summary of schema 2, and this window reads schema 1',
        );
        expect(notASummary({ ...savingsSummary(), week: null })).toBe('the savings summary has no periods');
        expect(notASummary({ ...savingsSummary(), allTime: undefined })).toBe('the savings summary has no periods');
    });

    it('refuses a figure that is not a number: JSON sends an engine NaN as null, which would read $0.00', () => {
        const bad = 'the savings summary holds a figure that is not a number';
        const nan = { low: 0, value: Number.NaN, high: 0 };
        const cases = [
            recordedPeriod(10, null, { measured: dollars(1, { net: { low: 0, value: null as unknown as number, high: 1 } }) }),
            recordedPeriod(10, null, { measured: dollars(1, { gross: nan }) }),
            recordedPeriod(10, null, { measured: dollars(1, { extraRounds: { low: 0, value: 0, high: Infinity } }) }),
            recordedPeriod(10, 1, { modelled: { rows: 1, value: nan } }),
            recordedPeriod(10, 1, { sharePct: Number.NaN }),
            recordedPeriod(10, 1, { tokensSaved: nan }),
            recordedPeriod(10, 1, { tokensSaved: null as never }),
        ];
        for (const week of cases) expect(notASummary(savingsSummary({ week }))).toBe(bad);
        expect(notASummary(savingsSummary({ allTime: cases[0] }))).toBe(bad);
    });
});
