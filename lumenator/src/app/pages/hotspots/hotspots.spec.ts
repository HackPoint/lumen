import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Hotspots } from './hotspots';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { ContextReport, FileHotspot } from '../../components/index';

/**
 * Hotspots is the one screen that makes no savings claim, and that is its whole
 * justification: it costs no tokens, intercepts nothing and forces no rounds, so unlike
 * every other figure in the product it cannot come out negative. The tests below hold it
 * to that — the copy must describe what was read, never what was saved.
 */
describe('Hotspots', () => {
  let fixture: ComponentFixture<Hotspots>;
  let bridge: FakeTauriBridge;

  function file(over: Partial<FileHotspot> = {}): FileHotspot {
    return {
      path: '/p/a.ts',
      name: 'a.ts',
      reads: 1,
      totalTokens: 100,
      sharePct: 10,
      lines: 50,
      unchangedRereads: 0,
      recommendation: null,
      ...over,
    };
  }

  function report(over: Partial<ContextReport> = {}): ContextReport {
    return {
      totalTokensRead: 1_000,
      distinctFiles: 3,
      topFiles: [file()],
      top10SharePct: 40,
      totalUnchangedRereads: 0,
      ...over,
    };
  }

  /**
   * Build the page with the report already resolved.
   *
   * Two microtask ticks rather than `whenStable()`: SessionService holds a live daemon
   * subscription, so the fixture is never "stable" and awaiting it hangs until the test
   * timeout. This is the same pattern the optimizer spec uses, for the same reason.
   */
  async function mount(r: ContextReport | null): Promise<void> {
    bridge = new FakeTauriBridge();
    if (r) bridge.responses.set('get_context_report', r);
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: TauriBridge, useValue: bridge },
        SessionService,
      ],
    });
    fixture = TestBed.createComponent(Hotspots);
    fixture.detectChanges();
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  it('fetches the report on init', async () => {
    await mount(report());
    expect(bridge.countOf('get_context_report')).toBe(1);
  });

  it('shows an empty state rather than zeros before anything is read', async () => {
    await mount(report({ totalTokensRead: 0, topFiles: [], distinctFiles: 0 }));
    expect(text()).toContain('No reads recorded yet');
    // A "0%" concentration would read as a measurement of nothing.
    expect(text()).not.toContain('Concentration');
  });

  it('survives a backend that is not there', async () => {
    bridge = new FakeTauriBridge();
    bridge.failures.add('get_context_report');
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Hotspots);
    expect(() => fixture.detectChanges()).not.toThrow();
    await Promise.resolve();
    fixture.detectChanges();
    expect(text()).toContain('No reads recorded yet');
  });

  it('ranks files and shows reads, lines and share', async () => {
    await mount(
      report({
        totalTokensRead: 18_394_470,
        distinctFiles: 1_189,
        top10SharePct: 40.1,
        topFiles: [
          file({ name: 'Run.tsx', path: '/p/Run.tsx', reads: 139, totalTokens: 3_833_250, sharePct: 20.8, lines: 3_833 }),
          file({ name: 'small.rs', path: '/p/small.rs', reads: 2, totalTokens: 900, sharePct: 0.1, lines: 40 }),
        ],
      }),
    );
    const t = text();
    expect(t).toContain('Run.tsx');
    expect(t).toContain('139 reads');
    expect(t).toContain('3,833 lines');
    expect(t).toContain('20.8% of all context');
    expect(t).toContain('3.83M');
    expect(t).toContain('1,189 files');
    expect(t).toContain('40%');
  });

  /** The framing rule: this screen reports what was read, not what was saved. */
  it('never claims a saving anywhere in its copy', async () => {
    await mount(
      report({
        topFiles: [
          file({ recommendation: '3833 lines read 139 times — splitting it saves more than any read optimisation can' }),
        ],
      }),
    );
    const t = text().toLowerCase();
    // "savings" itself is not banned — the lede uses it to disclaim one. What must not
    // appear is a claim to have produced a saving.
    for (const banned of ['we saved', 'saved you', '% saved', 'effectiveness', 'optimizer effectiveness']) {
      expect(t).not.toContain(banned);
    }
    expect(t).toContain('not a savings claim');
  });

  it('renders a recommendation only when the backend supplies one', async () => {
    await mount(report({ topFiles: [file({ recommendation: 'split this file' })] }));
    expect(text()).toContain('split this file');

    await mount(report({ topFiles: [file({ recommendation: null })] }));
    expect(fixture.nativeElement.querySelector('.hs__rec')).toBeNull();
  });

  it('shows the unchanged-reread share, not the raw count, and only when non-zero', async () => {
    await mount(report({ topFiles: [file({ reads: 6, unchangedRereads: 3 })] }));
    expect(text()).toContain('50% unchanged');

    // Scoped to the row: the summary stat is permanently labelled "Re-read unchanged",
    // so a page-wide assertion would be testing the heading, not the row.
    await mount(report({ topFiles: [file({ reads: 6, unchangedRereads: 0 })] }));
    const meta = fixture.nativeElement.querySelector('.hs__meta') as HTMLElement;
    expect(meta.textContent ?? '').not.toContain('unchanged');
  });

  it('omits the line count when the backend did not record one', async () => {
    await mount(report({ topFiles: [file({ lines: null })] }));
    const m = fixture.nativeElement.querySelector('.hs__meta') as HTMLElement;
    expect(m.textContent ?? '').not.toContain('lines');
  });

  it('scales bars against the largest file so the top row is full', async () => {
    await mount(
      report({
        topFiles: [
          file({ path: '/a', totalTokens: 1_000 }),
          file({ path: '/b', totalTokens: 250 }),
        ],
      }),
    );
    const c = fixture.componentInstance;
    expect(c.barPct(c.files()[0])).toBe(100);
    expect(c.barPct(c.files()[1])).toBe(25);
  });

  it('gives a zero-token file a visible minimum bar rather than none', async () => {
    await mount(report({ topFiles: [file({ totalTokens: 1_000 }), file({ path: '/z', totalTokens: 0 })] }));
    const c = fixture.componentInstance;
    expect(c.barPct(c.files()[1])).toBe(2);
  });

  it('does not divide by zero when a file has no reads', async () => {
    await mount(report({ topFiles: [file({ reads: 0, unchangedRereads: 0 })] }));
    expect(fixture.componentInstance.unchangedPct(file({ reads: 0 }))).toBe(0);
  });
});
