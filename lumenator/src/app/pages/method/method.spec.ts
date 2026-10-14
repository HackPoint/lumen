import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Method } from './method';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge, PRICE_TABLE, priceRow } from '../../tauri-bridge.fake';
import type { SavingsSummary } from '../../savings-summary';
import { band, callLine, dollars, modelled, recordedPeriod, savingsCounts, savingsPeriod, savingsSummary, unpriced } from '../../savings-summary.fake';

/** How it's measured: the rules, prices, calibration, every count and band, and the per-call ledger. */
describe('Method', () => {
    let fixture: ComponentFixture<Method>;
    let bridge: FakeTauriBridge;

    const NOTE = "Uncalibrated: token figures are cl100k_base counts, OpenAI's tokenizer, a proxy for Claude's.";
    const METHOD = {
        pricesAsOf: '2026-10-13',
        calibration: { calibrated: false, asOf: null, note: NOTE },
        week: 'the ISO week: Monday 2026-10-12 00:00 local time to now',
        counterfactual: 'COUNTERFACTUAL',
        rounds: 'ROUNDS',
        roundCost: 'ROUND COST',
        extraRounds: 'EXTRA ROUNDS',
        floors: 'FLOORS',
        parts: 'PARTS',
    };

    function full(over: Partial<SavingsSummary> = {}): SavingsSummary {
        const week = recordedPeriod(9_000, null, {
            tokensFull: band(10_000),
            sharePct: 90,
            measured: dollars(0.11, { rows: 2, gross: { low: 0.1, value: 0.12, high: 0.2 }, extraRounds: band(0.01), net: { low: 0.09, value: 0.11, high: 0.19 }, highOpen: true }),
            modelled: modelled(0.4, 1),
            unpriced: unpriced({ calls: 1, tokens: band(500), noSession: 1 }),
            usdShortfalls: ['Some turns recorded no speed: priced at the lower rate.'],
            counts: savingsCounts({ calls: 3, blockedReads: 2, blockedReadsBounded: 1, derivedPairs: 2, valveFaults: 5 }),
            dataQuality: { state: 'partial', reason: '1 row with no session recorded, so tokens and no dollars' },
        });
        const allTime = recordedPeriod(20_000, 0.5, {
            tokensFull: { low: 30_000, value: 32_000, high: 34_000 },
            sharePct: -2.5,
            modelled: modelled(0.9, 3),
        });
        return savingsSummary({
            billingMode: 'subscription',
            week,
            allTime,
            status: { state: 'working' },
            method: METHOD,
            ledger: {
                recent: [
                    callLine(),
                    callLine({
                        ts: '2026-10-14T10:00:00.250Z',
                        tool: 'compress_logs',
                        path: '(inline)',
                        savedAgainst: 'alreadyInContext',
                        tokensSaved: band(0),
                        extraRounds: 1,
                        extraRoundBasis: 'inlineText',
                        extraRoundEvidenced: true,
                        netUsd: band(-0.05),
                    }),
                    callLine({
                        ts: '2026-10-13T09:00:00Z',
                        pair: { ...callLine().pair, basis: 'noSession', model: null, rounds: null, roundsBound: null },
                        dollars: 'none',
                        netUsd: null,
                        valueUsd: null,
                        extraRoundUsd: null,
                    }),
                    callLine({
                        ts: '2026-10-12T09:00:00Z',
                        kind: 'blockedRead',
                        tool: null,
                        path: null,
                        tokensSaved: band(-300),
                        readCapped: true,
                        pair: { ...callLine().pair, rounds: null, roundsBound: null },
                        extraRounds: 1,
                        extraRoundBasis: 'blockedReadBounded',
                        extraRoundEvidenced: false,
                        netUsd: null,
                    }),
                    callLine({
                        ts: '2026-10-11T09:00:00Z',
                        kind: 'call',
                        tool: null,
                        modelledValueUsd: band(0.02),
                        pair: { ...callLine().pair, roundsBound: 'sessionEndUnrecorded' },
                    }),
                ],
                omitted: 7,
                topGains: [callLine()],
                topLosses: [],
            },
            ...over,
        });
    }

    async function mount(seed: (b: FakeTauriBridge) => void): Promise<Method> {
        bridge = new FakeTauriBridge();
        bridge.responses.set('lumen_prices', PRICE_TABLE);
        seed(bridge);
        TestBed.configureTestingModule({
            providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
        });
        fixture = TestBed.createComponent(Method);
        fixture.detectChanges();
        await settle();
        return fixture.componentInstance;
    }

    async function settle(): Promise<void> {
        for (let i = 0; i < 8; i++) await Promise.resolve();
        fixture.detectChanges();
    }

    function root(): HTMLElement {
        return fixture.nativeElement as HTMLElement;
    }

    function norm(s: string | null | undefined): string {
        return (s ?? '').replace(/\s+/g, ' ').trim();
    }

    /** A table's body rows, each as its cells' text. */
    function rows(table: Element): string[][] {
        return Array.from(table.querySelectorAll('tbody tr')).map((r) => Array.from(r.children).map((c) => norm(c.textContent)));
    }

    function tableAfter(headingText: string): Element {
        const h = Array.from(root().querySelectorAll('h3')).find((e) => e.textContent?.trim() === headingText);
        expect(h, headingText).toBeDefined();
        return root().querySelector(`table[aria-labelledby="${h!.id}"]`)!;
    }

    function figures(): string[][] {
        return rows(root().querySelector('[aria-labelledby="figures-title"] table')!);
    }

    function section(id: string): Element {
        return root().querySelector(`[aria-labelledby="${id}"]`)!;
    }

    afterEach(() => TestBed.resetTestingModule());

    it('gives every rule the summary states, under its name', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        const dl = root().querySelector('dl.method__rules')!;
        const terms = Array.from(dl.querySelectorAll('dt')).map((d) => norm(d.textContent));
        const texts = Array.from(dl.querySelectorAll('dd')).map((d) => norm(d.textContent));
        expect(terms.length).toBe(7);
        expect(texts).toEqual([METHOD.week, 'COUNTERFACTUAL', 'ROUNDS', 'ROUND COST', 'EXTRA ROUNDS', 'FLOORS', 'PARTS']);
        expect(root().querySelector('h1')?.textContent).toBe("How it's measured");
        expect(norm(root().querySelector('.method__lede')?.textContent))
            .toBe("The summary as of 2026-10-14 12:00:00 UTC, from this machine's ledger.");
    });

    it('says the prices\' date and source, the dollars\' basis, and lays the table out', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        const prices = section('prices-title');
        expect(norm(prices.querySelector('p')?.textContent)).toBe(
            'List prices per model, speed and cache duration, as of 2026-10-13, from https://platform.claude.com/docs/en/about-claude/pricing. '
            + 'The dollars on Home are at API prices: your billing is not per token, or has not been said.',
        );
        const table = rows(prices.querySelector('table')!);
        expect(table[0]).toEqual(['claude-opus-4-8US-only inference: 1.1 times', '$5.00', '$25.00', '$6.25', '$10.00', '$0.50']);
        expect(table[1][0]).toBe('claude-opus-4-8, fast modeUS-only inference: 1.1 times');
        // A model older than 4.6 costs its price wherever it ran: no note.
        expect(table.find((r) => r[0] === 'claude-haiku-4-5')).toEqual(['claude-haiku-4-5', '$1.00', '$5.00', '$1.25', '$2.00', '$0.10']);
        expect(table.at(-1)).toEqual(['claude-haiku-5-5Priced by prompt length.', 'no price']);
    });

    it('on the API says nothing of API prices, and a row with no price and no note says so', async () => {
        await mount((b) => {
            b.responses.set('lumen_savings_summary', full({ billingMode: 'api' }));
            b.responses.set('lumen_prices', {
                asOf: '2026-10-13',
                models: [{ ...priceRow('claude-x', [0, 0, 0, 0, 0]), prices: null, note: '' }],
            });
        });
        const prices = section('prices-title');
        expect(norm(prices.querySelector('p')?.textContent)).toBe(
            'List prices per model, speed and cache duration, as of 2026-10-13, from https://platform.claude.com/docs/en/about-claude/pricing.',
        );
        expect(rows(prices.querySelector('table')!)).toEqual([['claude-xNo price.', 'no price']]);
    });

    it('says when the price table has not loaded, and names no source', async () => {
        await mount((b) => {
            b.responses.set('lumen_savings_summary', full());
            b.failures.add('lumen_prices');
        });
        const prices = section('prices-title');
        expect(norm(prices.querySelector('p')?.textContent)).toBe(
            'List prices per model, speed and cache duration, as of 2026-10-13. '
            + 'The dollars on Home are at API prices: your billing is not per token, or has not been said.',
        );
        expect(norm(prices.querySelectorAll('p')[1]?.textContent)).toBe('The price table has not loaded.');
        expect(prices.querySelector('table')).toBeNull();
    });

    it('says once that the counts are uncalibrated, in the engine\'s words', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        const cal = section('cal-title');
        expect(Array.from(cal.querySelectorAll('p')).map((p) => norm(p.textContent))).toEqual([NOTE]);
        expect(cal.textContent?.match(/Uncalibrated/g)?.length).toBe(1);
    });

    it('says the stand-in tokenizer itself where the engine gives no note', async () => {
        const s = full();
        s.method.calibration = { calibrated: false, asOf: null, note: '' };
        await mount((b) => b.responses.set('lumen_savings_summary', s));
        expect(norm(section('cal-title').querySelector('p')?.textContent))
            .toBe("Uncalibrated: counts are cl100k_base, a stand-in for Claude's tokenizer.");
    });

    it('says when and how it was calibrated once it is', async () => {
        const s = full();
        s.method.calibration = { calibrated: true, asOf: '2026-11-01', note: 'Within 3% on the corpus.' };
        await mount((b) => b.responses.set('lumen_savings_summary', s));
        expect(Array.from(section('cal-title').querySelectorAll('p')).map((p) => norm(p.textContent)))
            .toEqual(['Calibrated as of 2026-11-01.', 'Within 3% on the corpus.']);
        TestBed.resetTestingModule();
        const t = full();
        t.method.calibration = { calibrated: true, asOf: null, note: 'Within 3%.' };
        await mount((b) => b.responses.set('lumen_savings_summary', t));
        expect(norm(section('cal-title').querySelector('p')?.textContent)).toBe('Calibrated.');
    });

    it('gives each figure with its band and each count, this week and all time', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        const t = figures();
        const row = (what: string) => t.find((r) => r[0] === what);
        expect(row('Tokens saved, net')).toEqual(['Tokens saved, net', '9,000', '20,000']);
        expect(row('Tokens the files held')).toEqual(['Tokens the files held', '10,000', '32,000 (band 30,000 to 34,000)']);
        // The share as `lumen report` gives it: kept out, so a saving is positive.
        expect(row('Share kept out of context')).toEqual(['Share kept out of context', '90.0%', '−2.5%']);
        expect(row('Measured net, at least (the bold figure)')).toEqual([
            'Measured net, at least (the bold figure)',
            '$0.11 (band $0.09 to $0.19, and the high end is not a bound)',
            '$0.50 (band $0.50 to $0.50)',
        ]);
        expect(row('Measured value of the tokens')?.[1]).toBe('$0.12 (band $0.10 to $0.20)');
        expect(row('Measured extra rounds')?.[1]).toBe('$0.01 (band $0.01 to $0.01)');
        expect(row('Modelled, not added')).toEqual([
            'Modelled, not added',
            '+$0.40 from 1 call (band +$0.40 to +$0.40)',
            '+$0.90 from 3 calls (band +$0.90 to +$0.90)',
        ]);
        expect(row('Tokens only: calls')?.[1]).toBe('1');
        expect(row('Lumen calls')?.[1]).toBe('3');
        expect(row('Calls with their own R and C')?.[1]).toBe('2');
        expect(row('Blocked Reads let through')?.[1]).toBe('2');
        expect(row('of them bounded, where no evidence says whose (modelled, at the most)')?.[1]).toBe('1');
        expect(row('Escape-valve faults behind them')?.[1]).toBe('5');
        expect(row('Data')).toEqual(['Data', 'partial: 1 row with no session recorded, so tokens and no dollars', 'complete']);
        expect(Array.from(section('figures-title').querySelectorAll('li')).map((l) => norm(l.textContent)))
            .toEqual(['Some turns recorded no speed: priced at the lower rate.']);
    });

    it('lays the per-call ledger out as tables: the best and worst, then the newest', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        expect(Array.from(section('ledger-title').querySelectorAll('h3')).map((h) => h.textContent))
            .toEqual(['Gained most, all time', 'Newest first']);
        const recent = tableAfter('Newest first');
        expect(Array.from(recent.querySelectorAll('thead th')).map((h) => h.textContent)).toEqual([
            'Time', 'Call', 'Tokens saved', 'Against', 'Net', 'Read by', 'Extra round',
        ]);
        expect(rows(recent)).toEqual([
            ['2026-10-14 11:00:00 UTC', 'smart_read /repo/src/lib.rs', '9,000', 'the file, as a Read returns it', '$0.11',
                "claude-opus-4-8, R 12 to the session's end", 'none'],
            ['2026-10-14 10:00:00 UTC', 'compress_logs (inline)', '0', 'text already in context (none)', '−$0.05',
                "claude-opus-4-8, R 12 to the session's end", '1: writing inline text out, on evidence'],
            ['2026-10-13 09:00:00 UTC', 'smart_read /repo/src/lib.rs', '9,000', 'the file, as a Read returns it', 'tokens only',
                'no dollars: noSession', 'none'],
            ['2026-10-12 09:00:00 UTC', 'blocked Read, then let through', '−300, capped at what a Read returns', 'the file, as a Read returns it',
                'tokens only', 'claude-opus-4-8', '1: a blocked Read no evidence attributes, modelled'],
            ['2026-10-11 09:00:00 UTC', '? /repo/src/lib.rs', '9,000', 'the file, as a Read returns it', '$0.11 (+$0.02 modelled)',
                "claude-opus-4-8, R 12 to the session's end, unrecorded (may be too long)", 'none'],
        ]);
        // A loss, in dollars where it has them and in tokens where it has none.
        expect(Array.from(recent.querySelectorAll('tr.is-loss')).length).toBe(2);
        expect(norm(section('ledger-title').querySelector('.method__dim')?.textContent)).toBe('7 older calls are not listed.');
    });

    it('an empty ledger says no Lumen activity was recorded, lists nothing, and its figures are none', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', savingsSummary({ week: savingsPeriod(), allTime: savingsPeriod() })));
        const ledger = section('ledger-title');
        expect(norm(ledger.querySelector('p')?.textContent)).toBe('No Lumen activity recorded yet.');
        expect(ledger.querySelector('table')).toBeNull();
        expect(ledger.querySelector('.method__dim')).toBeNull();
        const t = figures();
        expect(t.find((r) => r[0] === 'Data')).toEqual(['Data', 'nothing recorded', 'nothing recorded']);
        expect(t.find((r) => r[0] === 'Measured net, at least (the bold figure)')?.[1]).toBe('none measured');
        expect(t.find((r) => r[0] === 'Modelled, not added')?.[1]).toBe('none');
        expect(t.find((r) => r[0] === 'Share kept out of context')?.[1]).toBe('nothing read');
        expect(section('figures-title').querySelector('ul')).toBeNull();
    });

    it('a failed refresh keeps the last summary, said to be not up to date', async () => {
        await mount((b) => b.responses.set('lumen_savings_summary', full()));
        bridge.failures.add('lumen_savings_summary');
        TestBed.inject(SessionService).refreshSavingsSummary();
        await settle();
        expect(norm(root().querySelector('.method__stale')?.textContent)).toBe(
            'Not up to date: the latest refresh failed (fake: lumen_savings_summary failed). This is the summary as of 2026-10-14 12:00:00 UTC.',
        );
        expect(root().querySelector('.method__lede')).toBeNull();
        expect(root().querySelector('dl.method__rules')).not.toBeNull();
    });

    it('a failed load says so, and shows no figure', async () => {
        await mount((b) => b.failures.add('lumen_savings_summary'));
        expect(norm(root().querySelector('.method__stale')?.textContent)).toBe("Lumen's savings could not be read: fake: lumen_savings_summary failed.");
        expect(root().querySelector('table')).toBeNull();
    });

    it('before the first answer it says it is reading, claims nothing, and has nothing to lay out', async () => {
        const m = await mount((b) => b.hangs.add('lumen_savings_summary'));
        expect(norm(root().querySelector('.method__p')?.textContent)).toBe('Reading the ledger.');
        expect(root().textContent).not.toContain('$');
        expect([m.rules(), m.calibration(), m.figures(), m.shortfalls(), m.gains(), m.losses(), m.recent()])
            .toEqual([[], [], [], [], [], [], []]);
        expect([m.basis(), m.omitted(), m.asOf()]).toEqual([null, 0, '']);
    });
});
