import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Hotspots } from './hotspots';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { ContextReport, FaultReport, FileHotspot } from '../../components/index';

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
  // ── Fault report ────────────────────────────────────────────────────────────
  //
  // Filing publishes to a public tracker and cannot be undone, so these tests are
  // mostly about what must NOT happen: no send on mount, no send on preview, no
  // second send, and never a body the user was not shown.

  function faultReport(over: Partial<FaultReport> = {}): FaultReport {
    return {
      body: '### lumen 1.4.0 — retry escape valve fired 7x on 2 files\n\nbody text',
      title: 'lumen 1.4.0 — retry escape valve fired 7x on 2 files',
      fingerprint: 'ffd15312',
      kinds: 2,
      occurrences: 7,
      repo: 'HackPoint/lumen',
      ...over,
    };
  }

  /**
   * Flush the invoke promise chain, not just its first link.
   *
   * `then(set url)` and `finally(clear filing)` are separate microtasks, so two awaits
   * land between them: the URL is on screen while the button still says "Filing…". Six
   * drains the whole chain, and awaiting `whenStable()` is not an option — SessionService
   * holds a live daemon subscription, so the fixture is never stable.
   */
  async function tick(): Promise<void> {
    for (let i = 0; i < 6; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  function button(label: string): HTMLButtonElement | undefined {
    const all = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('button'),
    ) as HTMLButtonElement[];
    return all.find((b) => (b.textContent ?? '').trim().startsWith(label));
  }

  it('does not touch the fault backend until asked', async () => {
    await mount(report());
    expect(bridge.countOf('get_fault_report')).toBe(0);
    expect(bridge.countOf('file_fault_report')).toBe(0);
    expect(button('File issue')).toBeUndefined();
  });

  it('checking for faults renders locally and files nothing', async () => {
    await mount(report());
    bridge.responses.set('get_fault_report', faultReport());

    button('Check for faults')!.click();
    await tick();

    expect(bridge.countOf('get_fault_report')).toBe(1);
    expect(bridge.countOf('file_fault_report')).toBe(0);
    expect(text()).toContain('this is the exact text that will be filed');
    expect(text()).toContain('2 fault groups, 7 occurrences');
  });

  it('files only on the second, explicit click, and sends the shown body', async () => {
    await mount(report());
    const r = faultReport();
    bridge.responses.set('get_fault_report', r);
    bridge.responses.set('file_fault_report', 'https://github.com/HackPoint/lumen/issues/1');

    button('Check for faults')!.click();
    await tick();
    button('File issue')!.click();
    await tick();

    expect(bridge.countOf('file_fault_report')).toBe(1);
    // Byte-for-byte the text on screen: re-rendering could file something else.
    expect(bridge.lastArgsOf('file_fault_report')).toEqual({
      body: r.body,
      title: r.title,
      fingerprint: r.fingerprint,
      repo: r.repo,
    });
    expect(text()).toContain('https://github.com/HackPoint/lumen/issues/1');
  });

  it('will not file the same report twice', async () => {
    await mount(report());
    bridge.responses.set('get_fault_report', faultReport());
    bridge.responses.set('file_fault_report', 'https://example.test/1');

    button('Check for faults')!.click();
    await tick();
    button('File issue')!.click();
    await tick();

    const filed = button('Filed');
    expect(filed).toBeDefined();
    expect(filed!.disabled).toBe(true);

    filed!.click();
    await tick();
    expect(bridge.countOf('file_fault_report')).toBe(1);
  });

  it('says so when there is nothing to report, and offers no file button', async () => {
    await mount(report());
    bridge.responses.set('get_fault_report', null);

    button('Check for faults')!.click();
    await tick();

    expect(text()).toContain('No faults recorded');
    expect(button('File issue')).toBeUndefined();
  });

  it('surfaces a filing failure instead of claiming success', async () => {
    await mount(report());
    bridge.responses.set('get_fault_report', faultReport());
    bridge.failures.add('file_fault_report');

    button('Check for faults')!.click();
    await tick();
    button('File issue')!.click();
    await tick();

    expect(text()).toContain('fake: file_fault_report failed');
    expect(text()).not.toContain('Filed:');
  });

  it('dismissing drops the report without filing it', async () => {
    await mount(report());
    bridge.responses.set('get_fault_report', faultReport());

    button('Check for faults')!.click();
    await tick();
    button('Dismiss')!.click();
    await tick();

    expect(bridge.countOf('file_fault_report')).toBe(0);
    expect(text()).not.toContain('this is the exact text that will be filed');
  });
  it('badges the Hotspots tab only when faults are waiting', async () => {
    await mount(report());
    expect(
      (fixture.nativeElement as HTMLElement).querySelector('.tab-nav__badge'),
    ).toBeNull();

    bridge.responses.set('get_fault_count', 4);
    fixture.componentInstance.s.refreshFaultCount();
    await tick();

    const badge = (fixture.nativeElement as HTMLElement).querySelector('.tab-nav__badge');
    expect(badge).not.toBeNull();
    expect(badge!.textContent?.trim()).toBe('4');
  });

  it('puts the report section above the file list, not below it', async () => {
    await mount(report());
    const html = (fixture.nativeElement as HTMLElement).innerHTML;
    // Below a ten-row list in an 800x600 window it needs scrolling to find, which is
    // how it went unnoticed.
    expect(html.indexOf('Report a fault')).toBeLessThan(html.indexOf('hs__list'));
  });
});
