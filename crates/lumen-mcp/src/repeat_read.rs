//! `smart_read` and `recall_file` repeat reads: the glue between `lumen_core::repeat` and the
//! tools.
//!
//! The base is what this process returned earlier to one context, held in memory under the
//! (session, agent) the PreToolUse hook named for the call, so a subagent is told apart from its
//! parent and a call that cannot be placed gets the full answer. Whether the model still has the
//! base is answered from the transcript events the daemon records (`session_events`). What cannot
//! be known — a compaction inside a subagent, whose events the daemon skips as a sidechain, and
//! an event not yet ingested — is why every repeat answer tells the model how to ask again with
//! `fresh=true`.

use lumen_core::identity::Identity;
use lumen_core::meter::RankedMeta;
use lumen_core::ranked::Mode;
use lumen_core::repeat::{self, Base, Hash, OutlineBase, RecallBase, Sym};
use lumen_core::structure::CodeItem;
use serde_json::Value;

use super::{Outcome, caller, count_tokens, format_outline, metered_with};

enum Gate {
    /// Serve the full answer; the label is the plan recorded.
    Full(&'static str),
    /// A base exists and nothing known invalidates it.
    Repeat,
}

pub struct Ctx {
    /// Whose context the base belongs to. `None` when the call could not be placed: nothing is
    /// looked up for it and nothing is kept.
    who: Option<Identity>,
    key: String,
    tool: &'static str,
    mode: Mode,
    treated: bool,
    gate: Gate,
    base: Option<Base>,
    now: i64,
}

pub enum Served {
    Reply(Box<Outcome>),
    /// Fall through to the full answer, recording this as the plan.
    Fall(&'static str),
    No,
}

fn lock() -> std::sync::MutexGuard<'static, repeat::Store> {
    match repeat::store().lock() {
        Ok(g) => g,
        // A panic elsewhere cannot leave the map half-written in a way that matters: a base
        // is replaced whole, and a stale one only costs a repeat its saving.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// A call from `session` is about to be answered; `None` when it could not be placed. See
/// [`repeat::Store::enter`]: every base of another conversation goes first.
pub fn enter(session: Option<&str>) {
    let dropped = lock().enter(session);
    if dropped > 0 {
        let why = if session.is_some() {
            "the conversation changed"
        } else {
            "a call could not be placed"
        };
        eprintln!("lumen: {why}; {dropped} remembered read(s) dropped");
    }
}

/// How often a problem with the setting is written to the fault spool. The check runs on every
/// call; the row says it once in a while, so a broken file is visible without being a row per read.
const PROBLEM_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether repeat reads are on for the call being answered now: the environment, then the
/// setting file, then on (see [`lumen_core::settings`]). Read per call, so a change to the
/// setting applies to the next read. A setting that cannot be read is off, and says so.
///
/// Turning off also forgets what was remembered: a base kept before it was turned off would be
/// stale by the time it is turned on again, since reads in between were not tracked.
pub fn mode() -> Mode {
    let resolved = lumen_core::settings::repeat_reads();
    if let Some(problem) = &resolved.problem {
        let file = lumen_core::meter::db_path()
            .map(|db| lumen_core::settings::settings_path(&db))
            .unwrap_or_else(|| lumen_core::settings::FILE_NAME.into());
        lumen_core::settings::surface(problem, &file, PROBLEM_EVERY);
    }
    if resolved.mode == Mode::Off {
        let dropped = lock().enter(None);
        if dropped > 0 {
            eprintln!("lumen: repeat reads are off; {dropped} remembered read(s) dropped");
        }
    }
    resolved.mode
}

/// `None` when repeat reads are off and the caller did not pass `fresh` (off costs nothing).
///
/// A call that could not be placed in a conversation is answered in full and remembers nothing:
/// the base it would be compared with belongs to some conversation, and which one is not known.
pub fn begin(tool: &'static str, path: &str, req_key: &str, args: &Value) -> Option<Ctx> {
    let mode = mode();
    let fresh = args.get("fresh").and_then(Value::as_bool).unwrap_or(false);
    if mode == Mode::Off && !fresh {
        return None;
    }
    let who = caller();
    let key = format!("{tool}\0{req_key}");
    let base = match (&who, mode) {
        (Some(w), m) if m != Mode::Off => lock().get(&w.owner(), &key).cloned(),
        _ => None,
    };
    let gate = if fresh {
        Gate::Full(repeat::PLAN_FRESH)
    } else if who.is_none() {
        Gate::Full(repeat::PLAN_UNIDENTIFIED)
    } else if let (Some(b), Some(w)) = (&base, &who) {
        match lumen_core::meter::db_path() {
            None => Gate::Full("unverifiable"),
            Some(db) => match repeat::event_since(&db, &w.session, b.when_ms()) {
                Ok(None) => Gate::Repeat,
                Ok(Some(kind)) if kind == "rewind" => Gate::Full("rewind"),
                Ok(Some(kind)) if kind == "resync" => Gate::Full("resync"),
                Ok(Some(_)) => Gate::Full("compaction"),
                // No ledger to ask, or no events table in it: it cannot be said that nothing
                // happened, so the answer is the full one, and the plan says why.
                Err(_) => Gate::Full("unverifiable"),
            },
        }
    } else {
        Gate::Full(repeat::PLAN_FIRST)
    };
    Some(Ctx {
        treated: repeat::treated(mode, path),
        who,
        key,
        tool,
        mode,
        gate,
        base,
        now: repeat::now_ms(),
    })
}

impl Ctx {
    pub fn serve_outline(
        &self,
        path: &str,
        file: Hash,
        syms: &[Sym],
        items: &[CodeItem],
        lines: usize,
        full_tokens: usize,
    ) -> Served {
        if !self.treated || !matches!(self.gate, Gate::Repeat) {
            return Served::No;
        }
        let Some(Base::Outline(b)) = &self.base else {
            return Served::No;
        };
        let yardstick =
            || count_tokens(&format_outline(path, lines, full_tokens, items)).min(full_tokens);
        if b.file == file {
            let text = repeat::unchanged_text("smart_read");
            let returned = count_tokens(&text);
            if returned >= yardstick() {
                return Served::Fall("unchanged_not_smaller");
            }
            return Served::Reply(Box::new(self.reply(
                text,
                full_tokens,
                returned,
                repeat::ROUTE_UNCHANGED,
                repeat::PLAN_UNCHANGED,
                path,
                lines,
                None,
            )));
        }
        if b.syms.is_empty() {
            return Served::Fall("no_base");
        }
        let d = repeat::diff(&b.syms, syms);
        let text = repeat::format_delta(lines, &d);
        let returned = count_tokens(&text);
        if returned >= yardstick() {
            return Served::Fall("delta_too_big");
        }
        self.put(Base::Outline(OutlineBase {
            when_ms: self.now,
            file,
            lines,
            syms: syms.to_vec(),
        }));
        Served::Reply(Box::new(self.reply(
            text,
            full_tokens,
            returned,
            repeat::ROUTE_DELTA,
            repeat::PLAN_DELTA,
            path,
            lines,
            None,
        )))
    }

    /// Label a normally delivered outline and remember it as the base.
    pub fn stamp_outline(
        &self,
        o: &mut Outcome,
        fell: Option<&'static str>,
        file: Hash,
        syms: &[Sym],
        lines: usize,
    ) {
        let label = fell.unwrap_or_else(|| self.predicted(Some(file), None));
        let Some(m) = o.meter.as_mut() else {
            return;
        };
        self.label(m, label);
        if self.mode != Mode::Off {
            // Both whole-file fallbacks return at least the file, so a reply smaller than the
            // file is an outline. Anything else leaves nothing to diff against later.
            let outline_delivered = m.returned_tokens < m.full_tokens;
            self.put(Base::Outline(OutlineBase {
                when_ms: self.now,
                file,
                lines,
                syms: if outline_delivered {
                    syms.to_vec()
                } else {
                    vec![]
                },
            }));
        }
    }

    pub fn serve_recall(
        &self,
        path: &str,
        pieces: &[(Sym, String)],
        text: &str,
        full_tokens: usize,
        lines: usize,
        gutter: &dyn Fn(usize, &str) -> String,
    ) -> Served {
        if !self.treated || !matches!(self.gate, Gate::Repeat) {
            return Served::No;
        }
        let Some(Base::Recall(b)) = &self.base else {
            return Served::No;
        };
        let yardstick = count_tokens(text).min(full_tokens);
        let th = repeat::blob_hash(text);
        let (reply, route, plan, label) = if b.text == th {
            (
                repeat::unchanged_text(self.tool),
                repeat::ROUTE_RECALL_UNCHANGED,
                repeat::PLAN_UNCHANGED,
                "unchanged_not_smaller",
            )
        } else {
            (
                repeat::recall_delta(self.tool, b, pieces, gutter),
                repeat::ROUTE_RECALL_DELTA,
                repeat::PLAN_DELTA,
                "delta_too_big",
            )
        };
        let returned = count_tokens(&reply);
        if returned >= yardstick {
            return Served::Fall(label);
        }
        if plan == repeat::PLAN_DELTA {
            self.put_recall(th, pieces);
        }
        Served::Reply(Box::new(self.reply(
            reply,
            full_tokens,
            returned,
            route,
            plan,
            path,
            lines,
            Some(self.req_key().to_string()),
        )))
    }

    /// `pieces` is `None` for a recall with nothing to diff later (an outline, a too-broad
    /// match): it is labelled, and nothing is remembered.
    pub fn stamp_recall(
        &self,
        o: &mut Outcome,
        fell: Option<&'static str>,
        pieces: Option<&[(Sym, String)]>,
        text: &str,
    ) {
        let th = repeat::blob_hash(text);
        let label = fell.unwrap_or_else(|| self.predicted(None, Some(th)));
        let Some(m) = o.meter.as_mut() else {
            return;
        };
        self.label(m, label);
        if self.mode != Mode::Off
            && let Some(p) = pieces
        {
            self.put_recall(th, p);
        }
    }

    fn predicted(&self, file: Option<Hash>, text: Option<Hash>) -> &'static str {
        match (&self.gate, &self.base) {
            (Gate::Full(l), _) => l,
            (Gate::Repeat, Some(Base::Outline(b))) if Some(b.file) == file => {
                repeat::PLAN_UNCHANGED
            }
            (Gate::Repeat, Some(Base::Recall(b))) if Some(b.text) == text => repeat::PLAN_UNCHANGED,
            (Gate::Repeat, Some(_)) => repeat::PLAN_DELTA,
            (Gate::Repeat, None) => repeat::PLAN_FIRST,
        }
    }

    fn arm(&self) -> Option<&'static str> {
        match (self.mode, self.treated) {
            (Mode::Off, _) => None,
            (_, true) => Some(repeat::ARM_TREATMENT),
            (_, false) => Some(repeat::ARM_CONTROL),
        }
    }

    fn label(&self, m: &mut super::MeterRow, plan: &str) {
        m.ranked.repeat_arm = self.arm().map(Into::into);
        m.ranked.repeat_plan = Some(plan.into());
    }

    fn req_key(&self) -> &str {
        self.key.split_once('\0').map_or(&self.key, |(_, k)| k)
    }

    fn put(&self, base: Base) {
        // Nothing is kept for a call that could not be placed: there is no context to say the
        // model holds it.
        if let Some(who) = &self.who {
            lock().put(&who.owner(), &self.key, base);
        }
    }

    fn put_recall(&self, text: Hash, pieces: &[(Sym, String)]) {
        self.put(Base::Recall(RecallBase {
            when_ms: self.now,
            text,
            pieces: repeat::bound_pieces(pieces.to_vec()),
        }));
    }

    #[allow(clippy::too_many_arguments)]
    fn reply(
        &self,
        text: String,
        full_tokens: usize,
        returned: usize,
        route: &str,
        plan: &str,
        path: &str,
        lines: usize,
        req_key: Option<String>,
    ) -> Outcome {
        metered_with(
            text,
            full_tokens,
            returned,
            &format!("mcp__lumen__{}", self.tool),
            route,
            path,
            Some(lines as i64),
            req_key,
            RankedMeta {
                repeat_arm: self.arm().map(Into::into),
                repeat_plan: Some(plan.into()),
                ..Default::default()
            },
        )
    }
}
