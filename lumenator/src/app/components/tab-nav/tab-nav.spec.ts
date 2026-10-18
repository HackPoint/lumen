import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { TabNav } from './tab-nav';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { ContextReport, FileHotspot } from '../index';

describe('TabNav', () => {
  let fixture: ComponentFixture<TabNav>;
  let bridge: FakeTauriBridge;

  function file(over: Partial<FileHotspot> = {}): FileHotspot {
    return {
      path: '/p/a.ts',
      name: 'a.ts',
      reads: 1,
      totalTokens: 100,
      sharePct: 100,
      lines: 50,
      unchangedRereads: 0,
      recommendation: null,
      ...over,
    };
  }

  function report(files: FileHotspot[]): ContextReport {
    return { totalTokensRead: 100, distinctFiles: files.length, topFiles: files, top10SharePct: 100, totalUnchangedRereads: 0 };
  }

  async function mount(seed?: (b: FakeTauriBridge) => void): Promise<void> {
    bridge = new FakeTauriBridge();
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(TabNav);
    fixture.detectChanges();
    for (let i = 0; i < 6; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  function tabs(): string[] {
    return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('a.tab-nav__tab'))
      .map((a) => `${a.textContent?.trim()} ${a.getAttribute('href')}`);
  }

  afterEach(() => TestBed.resetTestingModule());

  it('names every screen, Home first, and leaves Why out when there is nothing to act on', async () => {
    await mount((b) => b.responses.set('get_context_report', report([file({ recommendation: '40% of everything' })])));
    expect(tabs()).toEqual([
      'Home /',
      'Optimizer /optimizer',
      'Details /details',
      'Settings /settings',
    ]);
  });

  it('shows Why, before Settings, once a file is something to act on', async () => {
    await mount((b) => b.responses.set('get_context_report', report([
      file({ reads: 12, unchangedRereads: 6, recommendation: '6 of 12 reads found the file unchanged' }),
    ])));
    expect(tabs()).toEqual([
      'Home /',
      'Optimizer /optimizer',
      'Details /details',
      'Why /why',
      'Settings /settings',
    ]);
  });

  it('leaves Why out when the report cannot be had', async () => {
    await mount((b) => b.failures.add('get_context_report'));
    expect(tabs().some((t) => t.startsWith('Why'))).toBe(false);
  });

  it('asks for the fault count and the context report once each', async () => {
    await mount();
    expect(bridge.countOf('get_fault_count')).toBe(1);
    expect(bridge.countOf('get_context_report')).toBe(1);
  });

  it('badges Settings with the recorded fault count, and nothing else', async () => {
    await mount((b) => b.responses.set('get_fault_count', 3));
    const badges = (fixture.nativeElement as HTMLElement).querySelectorAll('.tab-nav__badge');
    expect(badges.length).toBe(1);
    expect(badges[0].closest('a')?.getAttribute('href')).toBe('/settings');
    expect(badges[0].textContent?.trim()).toBe('3');
    expect(badges[0].getAttribute('aria-label')).toBe('3 faults recorded');
  });

  it('shows no badge when nothing was recorded', async () => {
    await mount((b) => b.responses.set('get_fault_count', 0));
    expect(fixture.nativeElement.querySelector('.tab-nav__badge')).toBeNull();
  });
});
