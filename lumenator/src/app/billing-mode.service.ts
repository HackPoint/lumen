import { Injectable, computed, inject, signal } from '@angular/core';
import { TauriBridge } from './tauri-bridge';

/** How the user pays for Claude Code. */
export type BillingMode = 'subscription' | 'api';

/** Mirrors `BillingModeSetting` in src-tauri/src/lib.rs. */
export interface BillingModeSetting {
  /** Null until the user has answered: not asked yet, never a guess at one of the two. */
  mode: BillingMode | null;
  /** Why the file's answer could not be used. While it is set, `mode` is null and nothing is claimed. */
  problem: string | null;
  /** The settings file, beside the ledger, so the text can name it. */
  file: string;
}

/** The two choices, in the order they are offered, with the words used for each. */
export const BILLING_CHOICES: ReadonlyArray<{ mode: BillingMode; label: string }> = [
  { mode: 'subscription', label: 'Subscription (Pro/Max)' },
  { mode: 'api', label: 'API (pay per token)' },
];

/**
 * How the user pays for Claude Code, as the settings file holds it, shared by Home's first-run
 * question and the Settings screen so both say the same thing about the same file.
 *
 * It decides what a saving is worth to the user: on the API a token not sent is money not spent,
 * on a subscription it is room under the plan's limits. Nothing reads it yet; the savings
 * summary will.
 */
@Injectable({ providedIn: 'root' })
export class BillingModeService {
  private readonly bridge = inject(TauriBridge);

  /** Reads and writes run one after another, so a read cannot overtake the write before it. */
  private tail: Promise<unknown> = Promise.resolve();

  /** What the file says; null until it was read, and again when it could not be. */
  readonly setting = signal<BillingModeSetting | null>(null);
  /** Why the setting could not be read. While this is set nothing is known, and nothing is claimed. */
  readonly readError = signal<string | null>(null);
  /** Why the last choice did not take. Cleared by the next read, so a reopened window starts clean. */
  readonly writeError = signal<string | null>(null);
  readonly saving = signal(false);

  /**
   * True only when the file was read, holds no answer and nothing is wrong with it. An unread or
   * unusable file is not "not asked": asking again then would talk over the problem.
   */
  readonly unasked = computed(() => {
    const s = this.setting();
    return s !== null && s.mode === null && s.problem === null;
  });

  load(): Promise<void> {
    return this.enqueue(async () => {
      this.readError.set(null);
      this.writeError.set(null);
      try {
        this.setting.set(asSetting(await this.bridge.getBillingModeSetting()));
      } catch (e) {
        this.setting.set(null);
        this.readError.set(describe(e));
      }
    });
  }

  /** Writes the choice, then shows what the file says. Resolves false when it did not take. */
  set(mode: BillingMode): Promise<boolean> {
    return this.enqueue(async () => {
      this.writeError.set(null);
      this.saving.set(true);
      try {
        const after = asSetting(await this.bridge.setBillingModeSetting(mode));
        this.setting.set(after);
        const mismatch = after.mode === mode
          ? null
          : `The settings file says ${after.mode ?? 'nothing'} about billing, not ${mode}.`
            + (after.problem ? ` ${after.problem}.` : '');
        this.writeError.set(mismatch);
        return mismatch === null;
      } catch (e) {
        this.writeError.set(describe(e));
        return false;
      } finally {
        this.saving.set(false);
      }
    });
  }

  private enqueue<T>(job: () => Promise<T>): Promise<T> {
    const run = this.tail.then(job, job);
    this.tail = run;
    return run;
  }
}

/** The answer if it is a setting, else an error: nothing is shown from a reply that is not one. */
function asSetting(v: unknown): BillingModeSetting {
  const s = v as Partial<BillingModeSetting> | null;
  const modeOk = s?.mode === null || s?.mode === 'subscription' || s?.mode === 'api';
  const problemOk = s?.problem === null || typeof s?.problem === 'string';
  if (typeof v !== 'object' || v === null || !modeOk || !problemOk || typeof s?.file !== 'string') {
    throw new Error('the app answered with something that is not a billing setting');
  }
  return { mode: s.mode as BillingMode | null, problem: s.problem as string | null, file: s.file };
}

function describe(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
