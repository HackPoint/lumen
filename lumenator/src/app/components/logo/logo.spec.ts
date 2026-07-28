import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Logo } from './logo';

describe('Logo', () => {
  let fixture: ComponentFixture<Logo>;

  function build(variant?: 'lockup' | 'mark'): Logo {
    fixture = TestBed.createComponent(Logo);
    if (variant) fixture.componentRef.setInput('variant', variant);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function html(): string {
    return (fixture.nativeElement as HTMLElement).innerHTML;
  }

  it('defaults to the full lockup', () => {
    expect(build().variant()).toBe('lockup');
  });

  it('renders the wordmark in the lockup variant', () => {
    build('lockup');
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Lumen');
  });

  it('omits the wordmark in the mark-only variant', () => {
    build('mark');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Lumen');
  });

  it('exposes the variant as a data attribute for CSS', () => {
    build('mark');
    const host = (fixture.nativeElement as HTMLElement).querySelector('.logo');
    expect(host?.getAttribute('data-variant')).toBe('mark');
  });

  it('gives each instance unique gradient ids', () => {
    // Shared SVG def ids would make a second logo inherit the first's gradients.
    const a = build();
    const ids = [a.arcId, a.coreId, a.glowId];
    const b = build();
    for (const [i, id] of [b.arcId, b.coreId, b.glowId].entries()) {
      expect(id).not.toBe(ids[i]);
    }
  });

  it('uses distinct ids for its three gradients', () => {
    const l = build();
    expect(new Set([l.arcId, l.coreId, l.glowId]).size).toBe(3);
  });

  it('is decorative, not announced to screen readers', () => {
    build();
    expect(html()).toContain('aria-hidden="true"');
  });
});
