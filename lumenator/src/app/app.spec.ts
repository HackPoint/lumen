import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { AppComponent } from './app.component';
import { appConfig } from './app.config';
import { Home } from './pages/home/home';
import { Hotspots } from './pages/hotspots/hotspots';
import { Optimizer } from './pages/optimizer/optimizer';
import { Panel } from './pages/panel/panel';
import { Setup } from './pages/setup/setup';
import { TauriBridge } from './tauri-bridge';
import { FakeTauriBridge } from './tauri-bridge.fake';

/**
 * The shell and the URLs the windows load. tauri.conf.json opens the panel at "/panel"
 * and the main window at "/", and Home sends a first run to "/setup": a route renamed
 * here leaves a window blank while every page spec stays green.
 */
describe('App shell', () => {
  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [
        ...appConfig.providers,
        { provide: TauriBridge, useValue: new FakeTauriBridge() },
      ],
    });
  });

  it('serves each URL a window loads its page, through the app config', async () => {
    const harness = await RouterTestingHarness.create();
    const pages = [
      ['/', Home],
      ['/optimizer', Optimizer],
      ['/hotspots', Hotspots],
      ['/panel', Panel],
      ['/setup', Setup],
    ] as const;
    for (const [url, page] of pages) {
      expect(await harness.navigateByUrl(url), url).toBeInstanceOf(page);
    }
  });

  it('renders the routed page once, inside the shell', async () => {
    const fixture = TestBed.createComponent(AppComponent);
    await TestBed.inject(Router).navigateByUrl('/hotspots');
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelectorAll('router-outlet').length).toBe(1);
    expect(root.querySelectorAll('hotspots').length).toBe(1);
    expect(root.querySelectorAll('home').length).toBe(0);
  });
});
