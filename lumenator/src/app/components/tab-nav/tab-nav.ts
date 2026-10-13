import { ChangeDetectionStrategy, Component, OnInit, computed, inject } from '@angular/core';
import { RouterLink, RouterLinkActive } from '@angular/router';
import { SessionService } from '../../session.service';
import { recommendations } from '../../recommendation';

/**
 * The main window's navigation, one copy for every full-window route.
 *
 * Each page used to carry its own copy of this markup, and the copies drifted: a tab added to
 * one page was missing from the next. With a tab that comes and goes (Why) and a badge that
 * moved (to Settings), one definition is the only way they stay the same everywhere.
 *
 * Its styles are global (`.tab-nav*` in styles.css), so they reach it from any page.
 */
@Component({
    selector: 'lumen-tab-nav',
    imports: [RouterLink, RouterLinkActive],
    templateUrl: './tab-nav.html',
    // No box of its own, so the nav is laid out by the page's column as it was when it was
    // written into each page.
    styles: ':host { display: contents; }',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TabNav implements OnInit {
    readonly s = inject(SessionService);

    /**
     * Why is in the navigation only while it has something to say: a screen that is always there
     * and usually empty trains people not to open it. See `isRecommendation` for the rule.
     */
    readonly showWhy = computed(() => recommendations(this.s.contextReport()).length > 0);

    ngOnInit(): void {
        // Both on every page that shows the navigation, so the badge and the Why tab are current
        // wherever the user is: neither is on the daemon stream.
        this.s.refreshFaultCount();
        this.s.refreshContextReport();
    }
}
