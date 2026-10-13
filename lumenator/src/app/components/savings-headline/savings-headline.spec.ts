import { ComponentFixture, TestBed } from '@angular/core/testing';
import { SavingsHeadline, SavingsSummary } from './savings-headline';
import type { SavingsPeriod } from '../../savings-summary';
import { modelled, recordedPeriod, savingsPeriod, savingsSummary, unpriced } from '../../savings-summary.fake';

/**
 * The headline renders what it is given and computes nothing. These pin the empty states,
 * which must not read as a figure, and the signs, which must not be softened.
 */
describe('SavingsHeadline', () => {
  let fixture: ComponentFixture<SavingsHeadline>;

  /** A period that recorded something, with `tokens` saved and `usd` net (null: not priced). */
  function period(tokens: number, usd: number | null, over: Partial<SavingsPeriod> = {}): SavingsPeriod {
    return recordedPeriod(tokens, usd, over);
  }

  function summary(week: SavingsPeriod, allTime: SavingsPeriod): SavingsSummary {
    return savingsSummary({ week, allTime, status: { state: 'working' } });
  }

  function mount(summary: SavingsSummary | null): SavingsHeadline {
    fixture = TestBed.createComponent(SavingsHeadline);
    fixture.componentRef.setInput('summary', summary);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function text(): string {
    return ((fixture.nativeElement as HTMLElement).textContent ?? '').replace(/\s+/g, ' ').trim();
  }

  /** Each period as it reads: label, tokens line, dollars line. */
  function periods(): string[] {
    return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.headline__period'))
      .map((p) => (p.textContent ?? '').replace(/\s+/g, ' ').trim());
  }

  afterEach(() => TestBed.resetTestingModule());

  it('renders nothing at all without a summary: no placeholder, no zero', () => {
    mount(null);
    expect((fixture.nativeElement as HTMLElement).childElementCount).toBe(0);
    expect(text()).toBe('');
  });

  it('renders nothing by default, before anything sets a summary', () => {
    fixture = TestBed.createComponent(SavingsHeadline);
    fixture.detectChanges();
    expect(text()).toBe('');
  });

  it('says no savings are recorded, in words and with no figure, when the summary has no event', () => {
    mount(savingsSummary({ week: savingsPeriod(), allTime: savingsPeriod() }));
    expect(text()).toBe('No savings recorded yet.');
    expect(fixture.nativeElement.querySelector('.headline__period')).toBeNull();
  });

  it('shows tokens and dollars, net and signed, for this week and all time', () => {
    mount(summary(period(12_345, 1.5), period(1_234_567, 250.4)));
    expect(periods()).toEqual([
      'This week +12,345 tokens saved, net +$1.50 saved, net at least',
      'All time +1,234,567 tokens saved, net +$250 saved, net at least',
    ]);
    // Bold: the two figures are the screen's answer.
    const strong = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('strong'))
      .map((s) => s.textContent);
    expect(strong).toEqual(['+12,345', '+$1.50', '+1,234,567', '+$250']);
  });

  it('shows a loss as a loss, never as zero', () => {
    mount(summary(period(-800, -0.42), period(300, -3)));
    expect(periods()).toEqual([
      'This week −800 tokens saved, net −$0.42 saved, net at least',
      'All time +300 tokens saved, net −$3.00 saved, net at least',
    ]);
    const losses = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.is-loss'))
      .map((s) => s.textContent);
    expect(losses).toEqual(['−800', '−$0.42', '−$3.00']);
  });

  it('says nothing was measured, rather than $0, when no dollars were', () => {
    mount(summary(period(10, null), period(10, null)));
    expect(periods()[0]).toBe('This week +10 tokens saved, net — saved, net none measured');
    expect(text()).not.toContain('$0');
  });

  it('shows modelled dollars beside the measured figure, labelled, never as it', () => {
    mount(summary(period(10, 2, { modelled: modelled(182) }), period(10, null, { modelled: modelled(5) })));
    expect(periods()[0]).toBe('This week +10 tokens saved, net +$2.00 saved, net at least +$182 modelled');
    // Modelled dollars alone are no measured figure.
    expect(periods()[1]).toBe('All time +10 tokens saved, net — saved, net none measured +$5.00 modelled');
    const bold = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('strong.headline__usd'))
      .map((s) => s.textContent);
    expect(bold).toEqual(['+$2.00', '—']);
  });

  it('says how many calls have tokens and no dollars', () => {
    mount(summary(period(10, 2, { unpriced: unpriced({ calls: 1473, noSession: 1473 }) }), period(10, 2)));
    expect(periods()[0]).toBe('This week +10 tokens saved, net +$2.00 saved, net at least 1,473 calls unpriced');
    expect(periods()[1]).not.toContain('unpriced');
  });

  it('keeps the sign of a dollar figure that rounds to nothing; only zero has none', () => {
    const h = mount(null);
    expect(h.fmtTokens(0)).toBe('0');
    expect(h.fmtTokens(-0.3)).toBe('0');
    expect(h.fmtUsd(0.004)).toBe('+$0.00');
    expect(h.fmtUsd(-0.004)).toBe('−$0.00');
    expect(h.fmtUsd(0)).toBe('$0.00');
    expect(h.fmtUsd(-0)).toBe('$0.00');
    expect(h.fmtUsd(-100)).toBe('−$100');
  });
});
