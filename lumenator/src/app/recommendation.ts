import type { ContextReport, FileHotspot } from './components/index';

// MIRRORS the first two rules of `recommend()` in crates/lumen-stats/src/lib.rs, which writes
// each file's advice. Keep the two in step: a threshold raised there and not here would leave
// a file this rule picks with no advice, and `isRecommendation` would then drop it.

/** A file this long, read this often, is worth splitting. */
const SPLIT_MIN_LINES = 1_000;
const SPLIT_MIN_READS = 20;
/** Read at least this often, at least half the time unchanged: re-acquired, not retained. */
const REACQUIRED_MIN_READS = 10;

/**
 * Whether a hotspot is something for the user to act on, which is what the Why screen is for and
 * what puts it in the navigation.
 *
 * The rule, over the figures every hotspot already carries:
 *   - a file of at least 1,000 lines read at least 20 times. Every read paid for the whole file,
 *     so splitting it saves more than any read optimisation can; or
 *   - a file read at least 10 times, at least half of them finding it unchanged since the read
 *     before. That context was read again in full instead of being kept.
 * and the backend wrote advice for it, so the screen never lists a file with nothing to say.
 *
 * `recommend()`'s third rule, one file holding 10% or more of everything read, is left out on
 * purpose. It describes the ledger rather than asking anything of the reader, and on a young
 * ledger nearly every file meets it, so it would keep Why in the navigation with nothing to do.
 * Its text is still shown beside that file whenever the screen is open.
 */
export function isRecommendation(f: FileHotspot): boolean {
  if (f.recommendation === null) return false;
  const split = f.lines !== null && f.lines >= SPLIT_MIN_LINES && f.reads >= SPLIT_MIN_READS;
  const reacquired = f.reads >= REACQUIRED_MIN_READS && f.unchangedRereads * 2 >= f.reads;
  return split || reacquired;
}

/** The files to act on, heaviest first as the report ranks them. None before the report loads. */
export function recommendations(report: ContextReport | null): FileHotspot[] {
  return report?.topFiles.filter(isRecommendation) ?? [];
}
