//! The capability map as gating decisions.
//!
//! `benchmarks/versions/manifest.toml` says, per version, what its binary can do. That map is a
//! claim, not a fact: this module turns it into the decisions the runners make (may `hook` be
//! spawned, is a saving clamped, which cells cannot exist) and into the checks that stop a run
//! when a probe disagrees with it. Nothing here runs a binary except [`Caps::probe_identity`],
//! and that one only where the map says the version has a `hook` subcommand.
//!
//! Rules that hold throughout:
//! - `lumen-mcp hook ...` is never spawned for a version the map says has no `hook` subcommand:
//!   on those binaries it silently starts an MCP server.
//! - A probe that disagrees with the map stops the run. The map is then corrected in a commit of
//!   its own; the harness never reads the disagreement as the truth.
//! - A build is identified by the sha256 of its asset, never by `serverInfo.version` (1.0.0
//!   reports "0.2.0").

use crate::driver::Identity;
use crate::manifest::VersionEntry;
use crate::util::Res;
use std::path::Path;

/// Whether a version has repeat reads, and how a run gets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatReads {
    /// Before 1.7.0: no stub, no delta.
    None,
    /// 1.7.0: behind `LUMEN_REPEAT_READS`, so measured with it on and labelled "opt-in".
    OptIn,
    /// From 1.8.0: on unless turned off.
    Default,
}

/// What the map says about how a build learns whose call it is serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityClaim {
    /// Before 1.6.0: nothing, and no `hook` subcommand to ask.
    None,
    /// 1.6.0 and 1.7.0: an environment value read at spawn.
    Env,
    /// From 1.8.0: a hook record per call.
    Hook,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// `1.9.0`, or `no-lumen`.
    pub version: String,
    /// Not a build at all: the arm where a file is read whole.
    pub no_lumen: bool,
    /// 1.0.0: the self-reported `saved_tokens` is floored at zero. Recorded, never used.
    pub saved_clamped: bool,
    /// The binary has a `hook` subcommand (1.6.0 and later).
    pub hook_cli: bool,
    pub bash_meter: bool,
    pub repeat: RepeatReads,
    pub identity: IdentityClaim,
    /// The asset's name and sha256 ([`crate::assets::provenance`]): what identifies the build.
    pub asset: Option<String>,
}

impl Caps {
    /// The map's claims for one version, checked for consistency with each other: a map that
    /// says "no `hook` subcommand" and "identity by hook" is wrong on its face.
    pub fn from_entry(version: &str, e: &VersionEntry) -> Res<Caps> {
        let repeat = match e.repeat_reads.as_str() {
            "none" => RepeatReads::None,
            "opt-in" => RepeatReads::OptIn,
            "default" => RepeatReads::Default,
            other => return Err(format!("{version}: repeat_reads {other:?} is not a value")),
        };
        let identity = match e.identity.as_str() {
            "none" => IdentityClaim::None,
            "env" => IdentityClaim::Env,
            "hook" => IdentityClaim::Hook,
            other => return Err(format!("{version}: identity {other:?} is not a value")),
        };
        let saved_clamped = match e.saved_tokens.as_str() {
            "clamped" => true,
            "signed" => false,
            other => return Err(format!("{version}: saved_tokens {other:?} is not a value")),
        };
        let c = Caps {
            version: version.to_string(),
            no_lumen: false,
            saved_clamped,
            hook_cli: e.hook_cli,
            bash_meter: e.bash_meter,
            repeat,
            identity,
            asset: None,
        };
        c.check_consistent()?;
        Ok(c)
    }

    /// The arm with no Lumen: no binary, so nothing here may spawn anything.
    pub fn no_lumen() -> Caps {
        Caps {
            version: "no-lumen".to_string(),
            no_lumen: true,
            saved_clamped: false,
            hook_cli: false,
            bash_meter: false,
            repeat: RepeatReads::None,
            identity: IdentityClaim::None,
            asset: None,
        }
    }

    fn check_consistent(&self) -> Res<()> {
        let v = &self.version;
        if (self.identity == IdentityClaim::None) == self.hook_cli {
            return Err(format!(
                "the map for {v} is inconsistent: identity is probed through `hook`, so a version \
                 has an identity mechanism (env or hook) exactly when it has the `hook` \
                 subcommand (hook_cli = {}, identity = {:?})",
                self.hook_cli, self.identity
            ));
        }
        if self.bash_meter && !self.hook_cli {
            return Err(format!(
                "the map for {v} says it has the Bash meter but no `hook` subcommand"
            ));
        }
        Ok(())
    }

    pub fn with_asset(mut self, provenance: String) -> Caps {
        self.asset = Some(provenance);
        self
    }

    /// May `lumen-mcp hook ...` be spawned for this build? Only where the map says it exists.
    pub fn may_spawn_hook(&self) -> bool {
        self.hook_cli && !self.no_lumen
    }

    /// The identity the build was expected to show when probed. `None` where no probe may run.
    pub fn expected_identity(&self) -> Option<Identity> {
        match self.identity {
            IdentityClaim::None => None,
            IdentityClaim::Env => Some(Identity::Env),
            IdentityClaim::Hook => Some(Identity::Hook),
        }
    }

    /// Stop when a probe found something other than the map says.
    pub fn check_identity(&self, found: Identity) -> Res<()> {
        match self.expected_identity() {
            Some(want) if want == found => Ok(()),
            Some(want) => Err(format!(
                "STOP: the capability map says {} identifies calls by {}, the probe found {}. \
                 The map is a claim the probes check; correct manifest.toml in a commit of its own",
                self.version,
                want.label(),
                found.label()
            )),
            None => Err(format!(
                "STOP: {} has no identity probe in the map, yet one ran and found {}",
                self.version,
                found.label()
            )),
        }
    }

    /// The identity of a build: asked of the build where the map says it has `hook`, and fixed
    /// at [`Identity::Env`] without spawning anything where it does not (the server then gets
    /// its session from the environment, which older binaries ignore). A probe that disagrees
    /// with the map is an error.
    pub fn probe_identity(&self, bin: &Path, root: &Path) -> Res<Identity> {
        if !self.may_spawn_hook() {
            return Ok(Identity::Env);
        }
        let found = crate::driver::probe_identity(bin, root)?;
        self.check_identity(found)?;
        Ok(found)
    }

    // --- n/a rules (benchmarks/versions/METHOD.md) ---------------------------------------------

    /// `hook.meter_bash`: before the Bash meter existed.
    pub fn na_bash_meter(&self) -> Option<String> {
        (!self.bash_meter).then(|| {
            "this version has no Bash meter hook (it arrived in 1.6.0 with `would_inflate`)"
                .to_string()
        })
    }

    /// `hook.per_call_identity` and `server.identity.*`: before identity was a hook.
    pub fn na_identity(&self) -> Option<String> {
        (self.identity != IdentityClaim::Hook).then(|| {
            match self.identity {
                IdentityClaim::None => {
                    "this version has no per-call identity (it arrived in 1.6.0 as an environment \
                 value and became a hook in 1.8.0)"
                }
                _ => {
                    "per-call identity is an environment value in this version, not a hook (a hook \
                 from 1.8.0)"
                }
            }
            .to_string()
        })
    }

    /// `server.repeat_*` and the stub and delta token figures: before repeat reads.
    pub fn na_repeat(&self) -> Option<String> {
        (self.repeat == RepeatReads::None)
            .then(|| "this version has no repeat reads (they arrived in 1.7.0)".to_string())
    }

    /// Every `hook.*` series of a version with no `hook` subcommand. Its hooks are bash and
    /// python3 scripts, which a later series times; spawning `hook` would start an MCP server.
    pub fn na_hook_subcommand(&self) -> Option<String> {
        (!self.hook_cli).then(|| {
            "this version has no `lumen-mcp hook` subcommand (it arrived in 1.6.0); its hooks are \
             bash and python3 scripts, and spawning `hook` on this binary would start an MCP \
             server instead"
                .to_string()
        })
    }

    /// The words the results carry about this build, beside its asset.
    pub fn labels(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.no_lumen {
            v.push(
                "no Lumen: tokens are the whole file (with the Read cap); hooks, daemon and \
                 server do not exist"
                    .to_string(),
            );
        }
        if !self.hook_cli && !self.no_lumen {
            v.push(format!(
                "hooks are {}",
                crate::versions::script_hooks::LABEL
            ));
        }
        if self.saved_clamped {
            v.push(
                "saved_tokens is clamped at zero in this version: the self-reported value is \
                 recorded and never used, the signed saving is recomputed from the re-counted \
                 reply"
                    .to_string(),
            );
        }
        if self.repeat == RepeatReads::OptIn {
            v.push("repeat reads: opt-in (measured with LUMEN_REPEAT_READS=on)".to_string());
        }
        v
    }
}

/// Whether a reply's own `saved_tokens` agrees with its `full_tokens` and `returned_tokens` as
/// the map says the version computes it: signed, `full - returned`, or clamped at zero.
pub fn saved_agrees(clamped: bool, full: i64, returned: i64, saved: i64) -> bool {
    let signed = full - returned;
    if clamped {
        saved == signed.max(0)
    } else {
        saved == signed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::VersionsManifest;

    fn caps_of(version: &str) -> Caps {
        let m = VersionsManifest::load_committed().unwrap();
        Caps::from_entry(version, m.get(version).unwrap()).unwrap()
    }

    #[test]
    fn the_committed_map_is_consistent_for_every_version() {
        let m = VersionsManifest::load_committed().unwrap();
        for v in &m.meta.versions {
            Caps::from_entry(v, m.get(v).unwrap()).unwrap_or_else(|e| panic!("{v}: {e}"));
        }
    }

    #[test]
    fn the_map_gates_hook_spawns_and_names_each_na_cell() {
        let old = caps_of("1.0.0");
        assert!(old.saved_clamped && !old.may_spawn_hook());
        assert!(old.na_hook_subcommand().is_some());
        assert!(old.na_bash_meter().is_some());
        assert!(old.na_identity().is_some());
        assert!(old.na_repeat().is_some());
        let v6 = caps_of("1.6.0");
        assert!(v6.may_spawn_hook() && !v6.saved_clamped);
        assert!(v6.na_hook_subcommand().is_none() && v6.na_bash_meter().is_none());
        assert!(v6.na_identity().is_some(), "env identity is not the hook");
        assert!(v6.na_repeat().is_some());
        let v7 = caps_of("1.7.0");
        assert_eq!(v7.repeat, RepeatReads::OptIn);
        assert!(v7.na_repeat().is_none());
        assert!(v7.labels().iter().any(|l| l.contains("opt-in")));
        let v8 = caps_of("1.8.0");
        assert!(v8.na_identity().is_none() && v8.expected_identity() == Some(Identity::Hook));
        assert!(
            !caps_of("1.9.0")
                .labels()
                .iter()
                .any(|l| l.contains("opt-in"))
        );
        let none = Caps::no_lumen();
        assert!(!none.may_spawn_hook() && none.no_lumen);
    }

    #[test]
    fn a_map_that_contradicts_itself_is_refused() {
        let m = VersionsManifest::load_committed().unwrap();
        let mut e = m.get("1.1.5").unwrap().clone();
        e.identity = "hook".to_string();
        assert!(Caps::from_entry("1.1.5", &e).is_err());
        let mut e = m.get("1.1.5").unwrap().clone();
        e.bash_meter = true;
        assert!(Caps::from_entry("1.1.5", &e).is_err());
        let mut e = m.get("1.1.5").unwrap().clone();
        e.repeat_reads = "sometimes".to_string();
        assert!(Caps::from_entry("1.1.5", &e).is_err());
    }

    #[test]
    fn a_probe_that_disagrees_with_the_map_stops_the_run() {
        let v8 = caps_of("1.8.0");
        v8.check_identity(Identity::Hook).unwrap();
        let e = v8.check_identity(Identity::Env).unwrap_err();
        assert!(
            e.contains("STOP") && e.contains("hook") && e.contains("env"),
            "{e}"
        );
        let v6 = caps_of("1.6.0");
        v6.check_identity(Identity::Env).unwrap();
        assert!(v6.check_identity(Identity::Hook).is_err());
        // A version the map gives no probe to cannot pass one.
        assert!(caps_of("1.0.0").check_identity(Identity::Env).is_err());
    }

    #[test]
    fn saved_tokens_is_judged_as_the_map_says_the_version_computes_it() {
        assert!(saved_agrees(false, 100, 30, 70));
        assert!(saved_agrees(false, 40, 70, -30));
        assert!(
            !saved_agrees(false, 40, 70, 0),
            "a signed build must not clamp"
        );
        assert!(saved_agrees(true, 100, 30, 70));
        assert!(saved_agrees(true, 40, 70, 0));
        assert!(
            !saved_agrees(true, 40, 70, -30),
            "a clamped build never reports a loss"
        );
        assert!(!saved_agrees(true, 100, 30, 5));
    }
}
