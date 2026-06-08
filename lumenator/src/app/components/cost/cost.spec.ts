import { ComponentFixture, TestBed } from '@angular/core/testing';

import { Cost } from './cost';

describe('Cost', () => {
  let component: Cost;
  let fixture: ComponentFixture<Cost>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [Cost]
    })
    .compileComponents();

    fixture = TestBed.createComponent(Cost);
    component = fixture.componentInstance;
    fixture.detectChanges();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });
});
