import { ComponentFixture, TestBed } from '@angular/core/testing';
import { SavingsHeadline, SavingsPeriod, SavingsSummary } from './savings-headline';

/**
 * The headline renders what it is given and computes nothing. These pin the empty states,
 * which must not read as a figure, and the signs, which must not be softened.
 */
describe('SavingsHeadline', () => {
  let fixture: ComponentFixture<SavingsHeadline>;

  function period(over: Partial<SavingsPeriod> = {}): SavingsPeriod {
    return { tokens: 0, usd: 0, usdModelled: false, events: 0, ...over };
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
    mount({ week: period(), allTime: period() });
    expect(text()).toBe('No savings recorded yet.');
    expect(fixture.nativeElement.querySelector('.headline__period')).toBeNull();
  });

  it('shows tokens and dollars, net and signed, for this week and all time', () => {
    mount({
      week: period({ tokens: 12_345, usd: 1.5, events: 4 }),
      allTime: period({ tokens: 1_234_567, usd: 250.4, events: 90 }),
    });
    expect(periods()).toEqual([
      'This week +12,345 tokens saved, net +$1.50 saved, net',
      'All time +1,234,567 tokens saved, net +$250 saved, net',
    ]);
    // Bold: the two figures are the screen's answer.
    const strong = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('strong'))
      .map((s) => s.textContent);
    expect(strong).toEqual(['+12,345', '+$1.50', '+1,234,567', '+$250']);
  });

  it('shows a loss as a loss, never as zero', () => {
    mount({
      week: period({ tokens: -800, usd: -0.42, events: 2 }),
      allTime: period({ tokens: 300, usd: -3, events: 5 }),
    });
    expect(periods()).toEqual([
      'This week −800 tokens saved, net −$0.42 saved, net',
      'All time +300 tokens saved, net −$3.00 saved, net',
    ]);
    const losses = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.is-loss'))
      .map((s) => s.textContent);
    expect(losses).toEqual(['−800', '−$0.42', '−$3.00']);
  });

  it('says a figure that could not be priced is not priced, rather than $0', () => {
    mount({ week: period({ tokens: 10, usd: null, events: 1 }), allTime: period({ tokens: 10, usd: null, events: 1 }) });
    expect(periods()[0]).toBe('This week +10 tokens saved, net — saved, net not priced');
    expect(text()).not.toContain('$0');
  });

  it('labels a modelled dollar figure as modelled', () => {
    mount({ week: period({ tokens: 10, usd: 2, usdModelled: true, events: 1 }), allTime: period({ events: 1 }) });
    expect(periods()[0]).toBe('This week +10 tokens saved, net +$2.00 saved, net modelled');
    expect(periods()[1]).not.toContain('modelled');
  });

  it('gives a figure that rounds to nothing no sign', () => {
    const h = mount(null);
    expect(h.fmtTokens(0)).toBe('0');
    expect(h.fmtTokens(-0.3)).toBe('0');
    expect(h.fmtUsd(0.004)).toBe('$0.00');
    expect(h.fmtUsd(-0.004)).toBe('$0.00');
    expect(h.fmtUsd(-100)).toBe('−$100');
  });
});
