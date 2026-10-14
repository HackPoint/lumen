import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { SavingsStatus, split } from './savings-status';
import type { Recommendation, SavingsStatus as Status, SavingsSummary } from '../../savings-summary';
import cases from '../../savings-text.cases.json';
import { caseSummary, CaseSummary, recordedPeriod, savingsSummary } from '../../savings-summary.fake';

/** Under the headline: one status line and at most one recommendation, with the step it implies. */
describe('SavingsStatus', () => {
    let fixture: ComponentFixture<SavingsStatus>;

    function mount(summary: SavingsSummary | null): void {
        TestBed.resetTestingModule();
        TestBed.configureTestingModule({ providers: [provideRouter([])] });
        fixture = TestBed.createComponent(SavingsStatus);
        fixture.componentRef.setInput('summary', summary);
        fixture.detectChanges();
    }

    function root(): HTMLElement {
        return fixture.nativeElement as HTMLElement;
    }

    function text(sel: string): string | null {
        const e = root().querySelector(sel);
        return e ? (e.textContent ?? '').replace(/\s+/g, ' ').trim() : null;
    }

    function with_(status: Status, recommendation: Recommendation | null = null): SavingsSummary {
        return savingsSummary({ week: recordedPeriod(10, 1), allTime: recordedPeriod(10, 1), status, recommendation });
    }

    afterEach(() => TestBed.resetTestingModule());

    it('renders nothing without a summary', () => {
        mount(null);
        expect(root().childElementCount).toBe(0);
    });

    it('says working, in the words lumen report uses', () => {
        mount(with_({ state: 'working' }));
        expect(text('.status__line')).toBe('Status: working');
        expect(root().querySelector('.status__line')?.getAttribute('data-state')).toBe('working');
        expect(root().querySelector('.status__rec')).toBeNull();
    });

    it('says not working, with the engine\'s reason, for a loss', () => {
        const c = cases.states.find((s) => s.name === 'netNegative')!;
        mount(caseSummary(c.summary as CaseSummary));
        expect(text('.status__line')).toBe('Status: not working: Lumen cost more than it saved: net loss of at most $0.42 this week');
        expect(text('.status__rec-text')).toBe(
            "This week's losing calls saved fewer tokens than the extra round each cost. LUMEN_LINE_THRESHOLD (300 lines by default) is the size from which a Read is sent to Lumen; a higher one sends only larger files.",
        );
        expect(root().querySelector('.status__btn')).toBeNull();
        expect(root().querySelector('.status__action')).toBeNull();
    });

    it('says a loss under half a cent whose band reaches a gain may not have been one', () => {
        const c = cases.states.find((s) => s.name === 'nearZeroNegative')!;
        mount(caseSummary(c.summary as CaseSummary));
        expect(text('.status__line')).toBe('Status: not working: Lumen may have cost more than it saved: net loss under half a cent this week');
    });

    it('says no data when there is none', () => {
        mount(with_({ state: 'noData' }));
        expect(text('.status__line')).toBe('Status: no data');
        expect(root().querySelector('.status__line')?.getAttribute('data-state')).toBe('noData');
    });

    it('gives blocked Reads with no saving their reason, and lumen doctor to run', () => {
        const c = cases.states.find((s) => s.name === 'blocksOnly')!;
        mount(caseSummary(c.summary as CaseSummary));
        expect(text('.status__line')).toMatch(/^Status: not working: in the last 7 days the Read intercept blocked 219 reads/);
        expect(Array.from(root().querySelectorAll('.status__rec-text code')).map((e) => e.textContent)).toEqual(['lumen doctor']);
        expect(text('.status__action')).toBe('In a terminal: lumen doctor');
        expect(root().querySelector('.status__btn')).toBeNull();
    });

    it('offers Setup for a hook that does not reach the server, and goes there', () => {
        mount(with_({ state: 'working' }, {
            code: 'identityHook',
            text: 'Lumen cannot tell which conversation its calls come from, so repeat reads are off and its calls carry tokens and no dollars. Run Setup in the Lumen app, or see `lumen doctor`.',
        }));
        const btn = root().querySelector<HTMLButtonElement>('.status__btn')!;
        expect(btn.textContent?.trim()).toBe('Run Setup');
        const go = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
        btn.click();
        expect(go).toHaveBeenCalledWith(['/setup']);
        expect(root().querySelector('.status__action')).toBeNull();
    });

    it('sends noCalls and unplacedCalls to lumen doctor too', () => {
        for (const code of ['noCalls', 'unplacedCalls'] as const) {
            mount(with_({ state: 'notWorking', reason: 'r' }, { code, text: 'Run `lumen doctor`: it says why.' }));
            expect(text('.status__action'), code).toBe('In a terminal: lumen doctor');
        }
    });

    it('splits backticked spans out as code, and leaves an unpaired backtick as text', () => {
        expect(split('Run `lumen doctor`: it says.')).toEqual([
            { text: 'Run ', code: false },
            { text: 'lumen doctor', code: true },
            { text: ': it says.', code: false },
        ]);
        expect(split('a ` b')).toEqual([{ text: 'a ` b', code: false }]);
    });
});
