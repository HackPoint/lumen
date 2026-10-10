//! `smart_read` repeat reads: the glue between `lumen_core::repeat` and the tool.

use lumen_core::meter::RankedMeta;
use lumen_core::ranked::Mode;
use lumen_core::repeat::{self, Delivered, ItemSig, Plan};
use lumen_core::structure::{CodeItem, detect_lang, outline};
use serde_json::Value;

use super::{Outcome, RepeatState, count_tokens, format_outline, metered_with, session_id};

pub struct Ctx {
    session: String,
    treated: bool,
    prev: Option<Delivered>,
    hash: String,
    sigs: Vec<ItemSig>,
    items: Vec<CodeItem>,
    lines: usize,
    tx_len: Option<u64>,
    pub plan: Plan,
}

pub enum Served {
    Reply(Box<Outcome>),
    /// Fall through to a normal outline, recording this as the plan.
    Fall(&'static str),
    No,
}

/// `None` when the flag is off, or when there is no session or ledger to key state on.
/// Off must cost nothing: this is the first thing every outline read calls.
pub fn begin(path: &str, src: &str, args: &Value) -> Option<Ctx> {
    let mode = repeat::mode_from_env();
    if mode == Mode::Off {
        return None;
    }
    let session = session_id()?;
    let db = lumen_core::meter::db_path()?;
    let items = outline(src, detect_lang(path));
    let hash = repeat::fingerprint(src);
    let sigs = repeat::item_sigs(src, &items);
    let prev = repeat::load(&db, &session, path);
    let refresh = args
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let plan = if refresh {
        Plan::Fresh("refresh")
    } else {
        let compacted = prev
            .as_ref()
            .and_then(|p| p.tx_len)
            .and_then(|at| repeat::compacted_since(&session, at));
        repeat::decide(prev.as_ref(), &hash, &sigs, compacted)
    };
    Some(Ctx {
        treated: repeat::treated(mode, path),
        tx_len: repeat::transcript_len(&session),
        lines: src.lines().count(),
        session,
        prev,
        hash,
        sigs,
        items,
        plan,
    })
}

impl Ctx {
    pub fn serve(&self, path: &str, full_tokens: usize, line_count: usize) -> Served {
        if !self.treated {
            return Served::No;
        }
        match &self.plan {
            Plan::Unchanged => {
                let text = repeat::format_unchanged(path, line_count, self.sigs.len(), &self.hash);
                let returned = count_tokens(&text);
                if returned >= full_tokens {
                    return Served::Fall("unchanged_not_smaller");
                }
                // No new state: the baseline stays the delivery the model actually holds.
                Served::Reply(Box::new(self.reply(
                    text,
                    full_tokens,
                    returned,
                    repeat::ROUTE_UNCHANGED,
                    "unchanged",
                    path,
                    line_count,
                    None,
                )))
            }
            Plan::Delta(d) => {
                let Some(prev) = &self.prev else {
                    return Served::No;
                };
                let text = repeat::format_delta(path, prev, &self.hash, self.lines, d);
                let returned = count_tokens(&text);
                // The yardstick is the outline the model would otherwise get.
                let outline_tokens =
                    count_tokens(&format_outline(path, line_count, full_tokens, &self.items));
                if returned >= outline_tokens || returned >= full_tokens {
                    return Served::Fall("delta_too_big");
                }
                Served::Reply(Box::new(self.reply(
                    text,
                    full_tokens,
                    returned,
                    repeat::ROUTE_DELTA,
                    "delta",
                    path,
                    line_count,
                    Some(self.delivered(true)),
                )))
            }
            Plan::Fresh(_) => Served::No,
        }
    }

    /// Label a normally delivered outline and remember what it was.
    pub fn stamp(&self, o: &mut Outcome, plan: &str) {
        let Some(m) = o.meter.as_mut() else {
            return;
        };
        m.ranked.repeat_arm = Some(self.arm().into());
        m.ranked.repeat_plan = Some(plan.into());
        // Both whole-file fallbacks return at least the file, so a reply smaller than the
        // file is an outline. Anything else leaves nothing to diff against later.
        let outline_delivered = m.returned_tokens < m.full_tokens;
        m.state = Some(RepeatState {
            session_id: self.session.clone(),
            path: m.path.clone(),
            delivered: self.delivered(outline_delivered),
        });
    }

    fn arm(&self) -> &'static str {
        if self.treated {
            repeat::ARM_TREATMENT
        } else {
            repeat::ARM_CONTROL
        }
    }

    fn delivered(&self, with_items: bool) -> Delivered {
        Delivered {
            hash: self.hash.clone(),
            lines: self.lines,
            items: if with_items {
                self.sigs.clone()
            } else {
                vec![]
            },
            tx_len: self.tx_len,
        }
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
        line_count: usize,
        state: Option<Delivered>,
    ) -> Outcome {
        let mut o = metered_with(
            text,
            full_tokens,
            returned,
            "mcp__lumen__smart_read",
            route,
            path,
            Some(line_count as i64),
            None,
            RankedMeta {
                repeat_arm: Some(self.arm().into()),
                repeat_plan: Some(plan.into()),
                ..Default::default()
            },
        );
        if let Some(m) = o.meter.as_mut() {
            m.state = state.map(|delivered| RepeatState {
                session_id: self.session.clone(),
                path: path.to_string(),
                delivered,
            });
        }
        o
    }
}
