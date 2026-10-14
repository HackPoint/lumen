import { Routes } from '@angular/router';
import { Home } from './pages/home/home';
import { Panel } from './pages/panel/panel';
import { Optimizer } from './pages/optimizer/optimizer';
import { Details } from './pages/details/details';
import { Why } from './pages/why/why';
import { Settings } from './pages/settings/settings';
import { Method } from './pages/method/method';
import { Setup } from './pages/setup/setup';

// "main" window loads "/" -> Home; "panel" window loads "/panel" -> Panel.
// The root AppComponent is a thin <router-outlet/> shell and is NOT itself a
// routed component, so nothing renders twice.
export const routes: Routes = [
  { path: '', component: Home },
  { path: 'optimizer', component: Optimizer },
  { path: 'details', component: Details },
  { path: 'why', component: Why },
  // Hotspots until 1.9.0: an old link lands on the screen that replaced it.
  { path: 'hotspots', redirectTo: 'why' },
  { path: 'settings', component: Settings },
  // How it's measured: reached from Home's headline and from Details, not a tab of its own.
  { path: 'method', component: Method },
  { path: 'panel', component: Panel },
  { path: 'setup', component: Setup },
];
