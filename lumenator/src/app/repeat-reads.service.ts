import { Injectable, inject, signal } from '@angular/core';
import { TauriBridge } from './tauri-bridge';
import type { RepeatReadsSetting } from './components/index';

/**
 * The repeat-reads switch as the settings file holds it, shared by the Home toggle and the
 * Optimizer card so both say the same thing about the same file.
 *
 * It never answers for the server. A `LUMEN_REPEAT_READS` value in a Claude Code MCP config
 * outranks the file and the app cannot see it, so what is held here is the file and the
 * default, nothing more.
 */
@Injectable({ providedIn: 'root' })
export class RepeatReadsService {
  private readonly bridge = inject(TauriBridge);

  /** Reads and writes run one after another, so a read cannot overtake the write before it. */
  private tail: Promise<unknown> = Promise.resolve();

  /** What the file says; null until it was read, and again when it could not be. */
  readonly setting = signal<RepeatReadsSetting | null>(null);
  /** Why the file could not be read. While this is set nothing is known, and nothing is claimed. */
  readonly readError = signal<string | null>(null);
  /** Why the last switch did not take. Cleared by the next read as well, so a reopened window starts clean. */
  readonly writeError = signal<string | null>(null);
  readonly saving = signal(false);

  load(): Promise<void> {
    return this.enqueue(async () => {
      this.readError.set(null);
      this.writeError.set(null);
      try {
        this.setting.set(asSetting(await this.bridge.getRepeatReadsSetting()));
      } catch (e) {
        this.setting.set(null);
        this.readError.set(describe(e));
      }
    });
  }

  /** Writes the choice, then shows what the file says. Resolves false when it did not take. */
  set(on: boolean): Promise<boolean> {
    return this.enqueue(async () => {
      this.writeError.set(null);
      this.saving.set(true);
      try {
        const after = asSetting(await this.bridge.setRepeatReadsSetting(on));
        this.setting.set(after);
        const mismatch = disagreement(on, after);
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

function asSetting(v: unknown): RepeatReadsSetting {
  if (typeof v !== 'object' || v === null || typeof (v as RepeatReadsSetting).on !== 'boolean') {
    throw new Error('the app answered with something that is not a setting');
  }
  return v as RepeatReadsSetting;
}

/** Why the file after a write is not what was asked for, or null when it is. */
function disagreement(asked: boolean, after: RepeatReadsSetting): string | null {
  if (after.problem) {
    return `The settings file was written but cannot be read back: ${after.problem}.`;
  }
  if (after.on !== asked) {
    return `The settings file says repeat reads are ${after.on ? 'on' : 'off'}, not ${asked ? 'on' : 'off'}. Something else may have rewritten it.`;
  }
  return null;
}

function describe(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
