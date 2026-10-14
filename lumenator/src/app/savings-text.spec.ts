import cases from './savings-text.cases.json';
import { savingsCounts } from './savings-summary.fake';
import {
    blockedReadsText,
    fixedAsRust,
    percent,
    shareText,
    tokensFigure,
    tokensPhrase,
    tokensSigned,
    tokensSpoken,
    usdParts,
    usdPhrase,
    usdPlus,
    usdSigned,
    usdSpoken,
} from './savings-text';

/**
 * The window formats every figure as `lumen` and the tray do: the same case list holds the Rust
 * formatters to these strings (crates/lumen-core/tests/frontend_savings_text.rs).
 */
describe('savings text', () => {
    it('formats every dollar figure in the shared case list as the engine does', () => {
        for (const c of cases.usd) {
            expect(usdSigned(c.x), `usdSigned(${c.x})`).toBe(c.signed);
            expect(usdPlus(c.x), `usdPlus(${c.x})`).toBe(c.plus);
            expect(usdPhrase(c.x), `usdPhrase(${c.x})`).toBe(c.phrase);
        }
    });

    it('formats every token figure in the shared case list as the engine does', () => {
        for (const c of cases.tokens) {
            expect(tokensSigned(c.n), `tokensSigned(${c.n})`).toBe(c.signed);
            expect(tokensFigure(c.n), `tokensFigure(${c.n})`).toBe(c.figure);
            expect(tokensPhrase(c.n), `tokensPhrase(${c.n})`).toBe(c.phrase);
        }
    });

    it('formats every share in the shared case list as the engine does', () => {
        for (const c of cases.share) {
            expect(shareText(c.pct), `shareText(${c.pct})`).toBe(c.text);
        }
    });

    it('rounds a tie half to even, as Rust does, where toFixed rounds it up', () => {
        // The values a cent apart between the window and the terminal before this.
        expect((0.125).toFixed(2)).toBe('0.13');
        expect(fixedAsRust(0.125, 2)).toBe('0.12');
        expect(fixedAsRust(0.375, 2)).toBe('0.38');
        expect(fixedAsRust(2.5, 0)).toBe('2');
        expect(fixedAsRust(3.5, 0)).toBe('4');
        expect(fixedAsRust(2.25, 1)).toBe('2.2');
        expect(fixedAsRust(9.995, 2)).toBe('9.99');
        expect(fixedAsRust(99.999, 2)).toBe('100.00');
        expect(fixedAsRust(999.5, 0)).toBe('1000');
        expect(fixedAsRust(0, 2)).toBe('0.00');
        expect(fixedAsRust(1e22, 0)).toBe('1e+22');
    });

    it('keeps zero unsigned, and never prints a figure that is not a number', () => {
        expect(usdSigned(0)).toBe('$0.00');
        expect(usdSigned(-0)).toBe('$0.00');
        expect(usdPlus(-0)).toBe('$0.00');
        expect(tokensSigned(-0)).toBe('0');
        for (const x of [Number.NaN, Infinity, -Infinity]) {
            expect(usdSigned(x)).toBe('no figure');
            expect(usdPlus(x)).toBe('no figure');
            expect(usdPhrase(x)).toBe('no figure');
            expect(usdSpoken(x)).toBe('no figure');
            expect(shareText(x)).toBe('no figure');
            expect(percent(x)).toBe('no figure');
        }
    });

    it('splits the measured net into the words around its bold figure', () => {
        expect(usdParts(1.5)).toEqual({ lead: 'at least', figure: '$1.50', tail: 'saved, net' });
        expect(usdParts(-0.42)).toEqual({ lead: 'net loss of at most', figure: '$0.42', tail: '' });
        expect(usdParts(-0.004)).toEqual({ lead: 'net loss', figure: 'under half a cent', tail: '' });
    });

    it('says each figure in words for a screen reader', () => {
        expect(usdSpoken(105.2)).toBe('at least 105 dollars saved, net');
        expect(usdSpoken(-0.42)).toBe('net loss of at most 0.42 dollars');
        expect(usdSpoken(-0.004)).toBe('net loss under half a cent');
        expect(usdSpoken(12_345.6)).toBe('at least 12,346 dollars saved, net');
        expect(tokensSpoken(12_345)).toBe('about 12,345 tokens saved, net');
        expect(tokensSpoken(-32)).toBe('about 32 tokens lost, net');
    });

    it('gives a share as lumen report does', () => {
        expect(percent(92.44)).toBe('92.4%');
        expect(percent(-2.5)).toBe('−2.5%');
    });

    it('says where blocked Reads come from only where they are not all of one kind on evidence', () => {
        const c = (t: number, f: number, b: number) =>
            savingsCounts({ blockedReads: t + f + b, blockedReadsFromTranscripts: t, blockedReadsFromFaults: f, blockedReadsBounded: b });
        expect(blockedReadsText(c(0, 0, 0))).toBeNull();
        expect(blockedReadsText(c(1, 0, 0))).toBe('1 blocked Read');
        expect(blockedReadsText(c(1200, 0, 0))).toBe('1,200 blocked Reads');
        expect(blockedReadsText(c(0, 9, 0))).toBe('9 blocked Reads');
        expect(blockedReadsText(c(3, 9, 0))).toBe('12 blocked Reads: 3 recorded, 9 from faults');
        expect(blockedReadsText(c(0, 9, 2))).toBe('11 blocked Reads: 9 from faults, 2 bounded');
    });
});
