import { Routes } from '@angular/router';
import { Home } from './pages/home/home';
import { Panel } from './pages/panel/panel';
import { Optimizer } from './pages/optimizer/optimizer';
import { Hotspots } from './pages/hotspots/hotspots';
import { Setup } from './pages/setup/setup';

// "main" window loads "/" -> Home; "panel" window loads "/panel" -> Panel.
// The root AppComponent is a thin <router-outlet/> shell and is NOT itself a
// routed component, so nothing renders twice.
export const routes: Routes = [
  { path: '', component: Home },
  { path: 'optimizer', component: Optimizer },
  { path: 'hotspots', component: Hotspots },
  { path: 'panel', component: Panel },
  { path: 'setup', component: Setup },
];
