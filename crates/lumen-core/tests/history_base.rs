//! A branch of this repository must descend from the scrubbed release history.
//!
//! `release/1.10.0` was rewritten once (tip `753c912`) to drop a name that must not be
//! published. A branch built on the history from before the rewrite shares no ancestor with it,
//! cannot be merged without `--allow-unrelated-histories`, and would publish the old history
//! again. So every checkout that has history must contain the rewritten tip.

use std::process::Command;

/// The tip of `release/1.10.0` after the rewrite.
const REWRITTEN_TIP: &str = "753c912c20ff68ae068bdd06a71b7b32aa3df321";

fn git(args: &[&str]) -> Option<(bool, String)> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

#[test]
fn the_branch_descends_from_the_rewritten_release_tip() {
    match git(&["rev-parse", "--is-shallow-repository"]) {
        // Not a checkout, no git, or a shallow clone: there is no history to judge.
        None | Some((false, _)) => return,
        Some((true, s)) if s == "true" => return,
        Some(_) => {}
    }
    // Only a branch still on the old history reaches the assert: its history lacks the tip.
    let ancestor =
        git(&["merge-base", "--is-ancestor", REWRITTEN_TIP, "HEAD"]).is_some_and(|(ok, _)| ok);
    assert!(
        ancestor,
        "HEAD does not contain the rewritten release tip {REWRITTEN_TIP}: rebuild the branch on it \
         (cherry-pick onto the tip); never merge it with --allow-unrelated-histories"
    );
}
