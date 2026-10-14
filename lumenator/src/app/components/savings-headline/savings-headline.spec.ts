import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { SavingsHeadline, SavingsSummary } from './savings-headline';
import cases from '../../savings-text.cases.json';
import { band, caseSummary, CaseSummary, dollars, modelled, recordedPeriod, savingsPeriod, savingsSummary, unpriced } from '../../savings-summary.fake';
import { usdSigned } from '../../savings-text';

/**
 * The headline renders what it is given and computes nothing. These pin every state it has on the
 * text it renders: the states and the pieces of each period come from the case list the terminal
 * and the tray are tested against too (`savings-text.cases.json`), so the window says what `lumen`
 * says.
 */
describe('SavingsHeadline', () => {
    let fixture: ComponentFixture<SavingsHeadline>;

    function mount(summary: SavingsSummary | null, error: string | null = null): SavingsHeadline {
        TestBed.resetTestingModule();
        TestBed.configureTestingModule({ providers: [provideRouter([])] });
        fixture = TestBed.createComponent(SavingsHeadline);
        fixture.componentRef.setInput('summary', summary);
        if (error !== null) fixture.componentRef.setInput('error', error);
        fixture.detectChanges();
        return fixture.componentInstance;
    }

    function root(): HTMLElement {
        return fixture.nativeElement as HTMLElement;
    }

    /** Whitespace collapsed; the no-break space after ≈ reads as a space here. */
    function norm(s: string | null | undefined): string {
        return (s ?? '').replace(/\s+/g, ' ').trim();
    }

    /** What the eye sees: the words for a screen reader and the band button left out. */
    function seen(el: Element = root()): string {
        const copy = el.cloneNode(true) as Element;
        copy.querySelectorAll('.visually-hidden, .info-btn').forEach((e) => e.remove());
        return norm(copy.textContent);
    }

    /** Each period box as the eye sees it. */
    function periods(): string[] {
        return Array.from(root().querySelectorAll('.headline__period')).map((p) => seen(p));
    }

    /** What a screen reader is given for each figure. */
    function spoken(): string[] {
        return Array.from(root().querySelectorAll('.headline__figure .visually-hidden')).map((e) => norm(e.textContent));
    }

    function bold(): string[] {
        return Array.from(root().querySelectorAll('strong')).map((s) => norm(s.textContent));
    }

    function losses(): string[] {
        return Array.from(root().querySelectorAll('.is-loss')).map((s) => norm(s.textContent));
    }

    /** A period box's pieces, named as the case list names them, text exactly as rendered. */
    function pieces(p: Element) {
        const one = (within: Element | null | undefined, sel: string) => within?.querySelector(sel)?.textContent?.trim() ?? null;
        const [tokens, usd] = Array.from(p.querySelectorAll('.headline__figure'));
        return {
            share: one(p, '.headline__share'),
            tokens: one(tokens, 'strong'),
            tokensUnit: one(tokens, '.headline__unit'),
            usdLead: one(usd, '.headline__floor'),
            usd: one(usd, 'strong'),
            usdTail: one(usd, '.headline__unit'),
            basis: one(p, '.headline__basis'),
            beside: Array.from(p.querySelectorAll('.headline__tag')).map((t) => t.textContent?.trim()),
            empty: tokens === undefined && /No Lumen activity/.test(seen(p)),
            partial: one(p, '.headline__partial')?.replace(/^Partial data: /, '').replace(/\.$/, '') ?? null,
        };
    }

    function state(name: string): SavingsSummary {
        const c = cases.states.find((s) => s.name === name);
        expect(c, name).toBeDefined();
        return caseSummary(c!.summary as CaseSummary);
    }

    afterEach(() => TestBed.resetTestingModule());

    // ── the shared case list ───────────────────────────────────────────────────────────────

    it('shows each state in the shared case list as `lumen` and the tray say it', () => {
        expect(cases.states.length).toBeGreaterThanOrEqual(12);
        for (const c of cases.states) {
            mount(caseSummary(c.summary as CaseSummary));
            const boxes = Array.from(root().querySelectorAll('.headline__period'));
            if (c.app.allTime.empty) {
                expect(boxes, c.name).toEqual([]);
                expect(seen(), c.name).toBe("No Lumen activity recorded yet. How it's measured");
            } else {
                expect(boxes.map(pieces), c.name).toEqual([c.app.week, c.app.allTime]);
            }
        }
    });

    // ── each state, as it reads ────────────────────────────────────────────────────────────

    it('API: tokens and dollars in bold, the dollars the measured net, "at least"', () => {
        mount(state('api'));
        expect(periods()).toEqual([
            'This week ≈ 12,345 tokens saved, net at least $1.50 saved, net',
            'All time ≈ 1,234,567 tokens saved, net at least $250 saved, net',
        ]);
        expect(bold()).toEqual(['≈ 12,345', '$1.50', '≈ 1,234,567', '$250']);
        expect(spoken()).toEqual([
            'about 12,345 tokens saved, net, this week',
            'at least 1.50 dollars saved, net, this week',
            'about 1,234,567 tokens saved, net, all time',
            'at least 250 dollars saved, net, all time',
        ]);
    });

    it('keeps ≈ and its number together: a no-break space, so they never wrap apart', () => {
        mount(state('subscription'));
        const raw = Array.from(root().querySelectorAll('strong.headline__tokens, .headline__share')).map((e) => e.textContent ?? '');
        expect(raw.length).toBe(4);
        for (const t of raw) expect(t.startsWith('≈ '), JSON.stringify(t)).toBe(true);
    });

    it('subscription: the share of tokens first, then tokens and dollars in bold, at API prices', () => {
        mount(state('subscription'));
        expect(periods()).toEqual([
            'This week ≈ 62% fewer tokens from file and tool reads this week ≈ 12,345 tokens saved, net at least $1.50 saved, net at API prices',
            'All time ≈ 58% fewer tokens from file and tool reads all time ≈ 1,234,567 tokens saved, net at least $250 saved, net at API prices',
        ]);
        expect(bold()).toEqual(['≈ 12,345', '$1.50', '≈ 1,234,567', '$250']);
        expect(spoken()[3]).toBe('at least 250 dollars saved, net, all time, at API prices');
    });

    it('billing not set or not readable reads as a subscription does: the share, then tokens and dollars at API prices', () => {
        for (const name of ['unset', 'unreadable']) {
            mount(state(name));
            expect(periods(), name).toEqual([
                'This week ≈ 62% fewer tokens from file and tool reads this week ≈ 12,345 tokens saved, net at least $1.50 saved, net at API prices',
                'All time ≈ 58% fewer tokens from file and tool reads all time ≈ 1,234,567 tokens saved, net at least $250 saved, net at API prices',
            ]);
        }
    });

    it('a loss reads as a loss, "net loss of at most", in the alert colour, never as zero', () => {
        mount(state('netNegative'));
        expect(periods()).toEqual([
            'This week ≈ 800 more tokens, net net loss of at most $0.42',
            'All time ≈ 300 tokens saved, net net loss of at most $3.00',
        ]);
        expect(losses()).toEqual(['≈ 800', '$0.42', '$3.00']);
        expect(spoken().slice(0, 2)).toEqual([
            'about 800 tokens lost, net, this week',
            'net loss of at most 0.42 dollars, this week',
        ]);
    });

    it('a loss under half a cent is said as one, in the alert colour', () => {
        mount(state('nearZeroNegative'));
        expect(periods()).toEqual([
            'This week ≈ 32 more tokens, net net loss under half a cent',
            'All time ≈ 5,000 tokens saved, net at least $0.00 saved, net',
        ]);
        expect(losses()).toEqual(['≈ 32', 'under half a cent']);
        expect(spoken()[1]).toBe('net loss under half a cent, this week');
    });

    it('partial data says why, in the period it is about', () => {
        mount(state('partial'));
        expect(periods()).toEqual([
            'This week ≈ 4,000 tokens saved, net at least $0.20 saved, net 2 calls unpriced Partial data: 2 rows with no session recorded, so tokens and no dollars.',
            'All time ≈ 9,000 tokens saved, net at least $0.60 saved, net 5 calls unpriced Partial data: 5 rows with no session recorded, so tokens and no dollars.',
        ]);
    });

    it('modelled dollars and unpriced calls stand beside the bold figure, smaller, never in it', () => {
        mount(state('modelledUnpriced'));
        expect(periods()).toEqual([
            'This week ≈ 10 tokens saved, net at least $2.00 saved, net +$182 modelled 1,473 calls unpriced Partial data: 1,473 rows with no session recorded, so tokens and no dollars.',
            // Modelled dollars alone are no measured figure: nothing bold, and no $0.
            'All time ≈ 10 tokens saved, net No dollars measured all time. +$5.00 modelled',
        ]);
        expect(bold()).toEqual(['≈ 10', '$2.00', '≈ 10']);
    });

    it('a week of blocked Reads alone says how many beside its figure', () => {
        mount(state('blocksOnly'));
        expect(periods()).toEqual([
            'This week ≈ 0 tokens saved, net net loss of at most $0.85 219 blocked Reads',
            'All time ≈ 50,000 tokens saved, net at least $1.20 saved, net',
        ]);
    });

    it('groups thousands of dollars, and shows the net where the gross is far from it', () => {
        mount(state('grossNotNetThousands'));
        expect(bold()).toEqual(['≈ 2,500,000', '$12,346', '≈ 9,876,543', '$1,234,567']);
        expect(bold().join(' ')).not.toContain('20,000');
    });

    it('a week with nothing in it says so, with no zero', () => {
        mount(state('weekEmpty'));
        expect(periods()[0]).toBe('This week No Lumen activity this week.');
        expect(root().querySelectorAll('.headline__period')[0].querySelector('strong, .info-btn')).toBeNull();
    });

    it('an empty ledger says no Lumen activity was recorded, in words and with no figure', () => {
        mount(state('empty'));
        expect(seen()).toBe("No Lumen activity recorded yet. How it's measured");
        expect(root().querySelector('.headline__period')).toBeNull();
        expect(root().textContent).not.toContain('$');
    });

    // ── the bold figure is measured.net ────────────────────────────────────────────────────

    it('the bold dollar figure is exactly measured.net: not the gross, the modelled or the unpriced', () => {
        const week = recordedPeriod(5_000, null, {
            measured: dollars(1.23, { gross: band(9.87), extraRounds: band(8.64), net: { low: 0.5, value: 1.23, high: 4.56 } }),
            modelled: modelled(7.77),
            unpriced: unpriced({ calls: 3, tokens: band(4_000) }),
        });
        mount(savingsSummary({ week, allTime: week, status: { state: 'working' } }));
        const usd = Array.from(root().querySelectorAll('strong.headline__usd')).map((s) => s.textContent);
        expect(usd).toEqual([usdSigned(1.23), usdSigned(1.23)]);
        expect(usd).toEqual(['$1.23', '$1.23']);
        for (const other of ['$9.87', '$8.64', '$7.77', '$0.50', '$4.56', '$9.00']) {
            expect(bold().join(' ')).not.toContain(other);
        }
        // Beside it, smaller: what it leaves out.
        expect(pieces(root().querySelector('.headline__period')!).beside).toEqual(['+$7.77 modelled', '3 calls unpriced']);
    });

    it('gives the bands behind the central figures on the period\'s ⓘ, and the figure stays central', () => {
        const week = recordedPeriod(5_000, null, {
            tokensSaved: { low: 4_000, value: 5_000, high: 6_000 },
            measured: dollars(1.23, { net: { low: 0.5, value: 1.23, high: 4.56 }, highOpen: true }),
        });
        const s = savingsSummary({ week, allTime: recordedPeriod(10, null), status: { state: 'working' } });
        mount(s);
        const info = Array.from(root().querySelectorAll('.headline__period .info-btn'));
        expect(info[0].getAttribute('aria-label')).toBe(
            "This week. Tokens saved: band 4,000 to 6,000, in cl100k_base counts, a stand-in for Claude's tokenizer; "
            + 'the band leaves out how the two differ. Dollars, net: $0.50 to $4.56, and the high end is not a bound. '
            + "The figures shown are the central ones; How it's measured has every band.",
        );
        expect(info[1].getAttribute('aria-label')).toBe(
            "All time. Tokens saved: band 10 to 10, in cl100k_base counts, a stand-in for Claude's tokenizer; "
            + "the band leaves out how the two differ. The figures shown are the central ones; How it's measured has every band.",
        );
        expect(bold()[0]).toBe('≈ 5,000');
        // Calibrated counts have a band of their own, and say no more.
        mount({ ...s, method: { ...s.method, calibration: { calibrated: true, asOf: '2026-11-01', note: '' } } });
        expect(root().querySelector('.headline__period .info-btn')?.getAttribute('aria-label'))
            .toMatch(/^This week\. Tokens saved: band 4,000 to 6,000\. Dollars/);
    });

    it('links to How it\'s measured', () => {
        mount(state('api'));
        const a = root().querySelector<HTMLAnchorElement>('a.headline__how')!;
        expect(a.textContent?.trim()).toBe("How it's measured");
        expect(a.getAttribute('href')).toBe('/method');
    });

    // ── before, and when fetching fails ────────────────────────────────────────────────────

    it('renders nothing at all without a summary: no placeholder, no zero', () => {
        mount(null);
        expect(root().childElementCount).toBe(0);
        expect(seen()).toBe('');
    });

    it('renders nothing by default, before anything sets a summary', () => {
        TestBed.configureTestingModule({ providers: [provideRouter([])] });
        fixture = TestBed.createComponent(SavingsHeadline);
        fixture.detectChanges();
        expect(seen()).toBe('');
    });

    it('a failed first load says so, with no figure', () => {
        mount(null, 'cannot open /x/lumen.db: unable to open database file');
        expect(seen()).toBe("Lumen's savings could not be read: cannot open /x/lumen.db: unable to open database file.");
        expect(root().querySelector('strong')).toBeNull();
        expect(root().textContent).not.toContain('$');
        expect(root().querySelector('[role="status"]')).not.toBeNull();
    });

    it('a failed refresh keeps the last good figures, marked as not up to date and when they are from', () => {
        mount(state('api'), 'fake: lumen_savings_summary failed');
        const stale = root().querySelector('.headline__stale')!;
        expect(norm(stale.textContent)).toBe(
            'Not up to date: the latest refresh failed (fake: lumen_savings_summary failed). These figures are as of 2026-10-14 12:00:00 UTC.',
        );
        expect(stale.getAttribute('role')).toBe('status');
        expect(bold()).toEqual(['≈ 12,345', '$1.50', '≈ 1,234,567', '$250']);
    });

    it('gives the screen a heading, and each period one of its own', () => {
        mount(state('api'));
        expect(root().querySelector('h2')?.textContent).toBe('What Lumen saved');
        expect(Array.from(root().querySelectorAll('h3')).map((h) => h.textContent)).toEqual(['This week', 'All time']);
        for (const section of Array.from(root().querySelectorAll('section.headline__period'))) {
            const id = section.getAttribute('aria-labelledby')!;
            expect(root().querySelector(`#${id}`)?.tagName).toBe('H3');
        }
        // The symbols are hidden from a screen reader, which is given the words instead.
        for (const s of Array.from(root().querySelectorAll('.headline__seen'))) {
            expect(s.getAttribute('aria-hidden')).toBe('true');
        }
    });

    it('a period that recorded nothing but has a summary of its own reads as nothing', () => {
        mount(savingsSummary({ week: savingsPeriod(), allTime: recordedPeriod(10, 0.1), status: { state: 'working' } }));
        expect(periods()[0]).toBe('This week No Lumen activity this week.');
    });
});
