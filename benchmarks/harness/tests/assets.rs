//! Release assets: verification, the cache rule and extraction, on fixtures. No network: a
//! stand-in downloader writes the bytes, and no real release is touched.

use lumen_bench::assets::{self, Source, ar_members, ensure_outside_repo, extract, fetch_with};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: Source<'static> = Source {
    repo: "HackPoint/lumen",
    tag: "v0.0.0",
};

fn sha(bytes: &[u8]) -> String {
    lumen_bench::util::sha256_hex(bytes)
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A tree holding the three sidecars (as scripts that must never be run) under `rel`.
fn sidecar_tree(root: &Path, rel: &str) {
    let d = root.join(rel);
    std::fs::create_dir_all(&d).unwrap();
    for s in assets::SIDECARS {
        std::fs::write(d.join(s), format!("not a binary: {s}\n")).unwrap();
    }
}

fn tar_gz(dir: &Path, out: &Path) {
    let st = Command::new("tar")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(dir)
        .arg(".")
        .status()
        .unwrap();
    assert!(st.success());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A zip of stored (uncompressed) entries.
fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let mut local = Vec::new();
        local.extend(0x0403_4b50u32.to_le_bytes());
        local.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, method, time, date
        local.extend(crc.to_le_bytes());
        local.extend((data.len() as u32).to_le_bytes());
        local.extend((data.len() as u32).to_le_bytes());
        local.extend((name.len() as u16).to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(name.as_bytes());
        out.extend(&local);
        out.extend(*data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend(crc.to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0u8; 8]); // extra and comment lengths, disk, internal attrs
        central.extend(0u32.to_le_bytes()); // external attrs
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn ar_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let hdr = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{name}/"),
            0,
            0,
            0,
            "100644",
            data.len()
        );
        assert_eq!(hdr.len(), 60);
        out.extend(hdr.as_bytes());
        out.extend(*data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

fn deb_fixture(dir: &Path) -> PathBuf {
    let tree = dir.join("tree");
    sidecar_tree(&tree, "usr/bin");
    std::fs::create_dir_all(tree.join("usr/share")).unwrap();
    std::fs::write(tree.join("usr/share/readme"), "x").unwrap();
    let data = dir.join("data.tar.gz");
    tar_gz(&tree, &data);
    let deb = dir.join("lumen_0.0.0_amd64.deb");
    std::fs::write(
        &deb,
        ar_bytes(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.gz", b"ctl"),
            ("data.tar.gz", &std::fs::read(&data).unwrap()),
        ]),
    )
    .unwrap();
    deb
}

fn names(files: &[PathBuf]) -> Vec<String> {
    let mut v: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// --- verification and the cache --------------------------------------------------------------

#[test]
fn a_sha_mismatch_refuses_and_keeps_nothing() {
    let cache = tempfile::tempdir().unwrap();
    let want = sha(b"the real asset");
    let dl = |_: Source<'_>, _: &str, dest: &Path| -> Result<(), String> {
        std::fs::write(dest, b"something else").map_err(|e| e.to_string())
    };
    let err = fetch_with(cache.path(), &repo(), SRC, "a.tar.gz", &want, &dl).unwrap_err();
    assert!(err.contains("REFUSED") && err.contains(&want), "{err}");
    assert!(
        !cache
            .path()
            .join("assets")
            .join(&want)
            .join("a.tar.gz")
            .exists(),
        "a refused file must not be cached"
    );
    assert!(
        !cache
            .path()
            .join("assets")
            .join(&want)
            .join("a.tar.gz.part")
            .exists(),
        "the partial file must not be left behind"
    );
}

#[test]
fn a_matching_asset_is_cached_and_reused_without_downloading_again() {
    let cache = tempfile::tempdir().unwrap();
    let bytes = b"the real asset";
    let want = sha(bytes);
    let calls = Cell::new(0);
    let dl = |_: Source<'_>, _: &str, dest: &Path| -> Result<(), String> {
        calls.set(calls.get() + 1);
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    };
    let a = fetch_with(cache.path(), &repo(), SRC, "a.zip", &want, &dl).unwrap();
    let b = fetch_with(cache.path(), &repo(), SRC, "a.zip", &want, &dl).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        (a.sha256.as_str(), a.name.as_str()),
        (want.as_str(), "a.zip")
    );
    assert_eq!(a.path, b.path);
    // A damaged cache file is not trusted: it is fetched again.
    std::fs::write(&a.path, b"damaged").unwrap();
    fetch_with(cache.path(), &repo(), SRC, "a.zip", &want, &dl).unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(std::fs::read(&a.path).unwrap(), bytes);
}

#[test]
fn an_unpinned_or_pathlike_request_never_reaches_the_downloader() {
    let cache = tempfile::tempdir().unwrap();
    let dl =
        |_: Source<'_>, _: &str, _: &Path| -> Result<(), String> { panic!("must not download") };
    let ok = sha(b"x");
    assert!(fetch_with(cache.path(), &repo(), SRC, "a.zip", "abc123", &dl).is_err());
    for bad in ["../a.zip", "d/a.zip", "..", ""] {
        assert!(
            fetch_with(cache.path(), &repo(), SRC, bad, &ok, &dl).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_cache_inside_the_repo_is_refused() {
    let r = repo();
    let dl =
        |_: Source<'_>, _: &str, _: &Path| -> Result<(), String> { panic!("must not download") };
    let ok = sha(b"x");
    for inside in [
        r.join("benchmarks/cache"),
        r.join("target/x"),
        r.join("benchmarks/../benchmarks/x"),
        r.clone(),
    ] {
        let err = fetch_with(&inside, &r, SRC, "a.zip", &ok, &dl).unwrap_err();
        assert!(err.contains("inside the repository"), "{err}");
    }
    assert!(
        !r.join("benchmarks/cache").exists(),
        "nothing may be created inside the repo"
    );
    // A symlink out of the temp dir into the repo does not hide it.
    #[cfg(unix)]
    {
        let t = tempfile::tempdir().unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&r, &link).unwrap();
        assert!(ensure_outside_repo(&link.join("sub"), &r).is_err());
    }
    let outside = tempfile::tempdir().unwrap();
    ensure_outside_repo(&outside.path().join("not-yet"), &r).unwrap();
}

#[test]
fn provenance_is_the_name_and_the_hash() {
    let p = assets::provenance("Lumen_1.9.0_aarch64.dmg", &"a".repeat(64));
    assert!(p.contains("Lumen_1.9.0_aarch64.dmg") && p.contains(&"a".repeat(64)));
    assert!(!p.contains('/'));
}

// --- extraction ------------------------------------------------------------------------------

#[test]
fn a_tar_gz_is_unpacked() {
    let t = tempfile::tempdir().unwrap();
    let tree = t.path().join("tree");
    std::fs::create_dir_all(tree.join("bin")).unwrap();
    std::fs::write(tree.join("bin/lumen"), "cli").unwrap();
    let a = t.path().join("lumen-v0-x86_64.tar.gz");
    tar_gz(&tree, &a);
    let dest = t.path().join("out");
    let files = extract(&a, "lumen-v0-x86_64.tar.gz", &dest).unwrap();
    assert_eq!(names(&files), ["lumen"]);
    assert_eq!(
        std::fs::read_to_string(dest.join("bin/lumen")).unwrap(),
        "cli"
    );
}

#[test]
fn a_zip_is_unpacked() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("lumen-v0-x64.zip");
    std::fs::write(&a, zip_bytes(&[("lumen.exe", b"cli"), ("README", b"hi")])).unwrap();
    let dest = t.path().join("out");
    let files = extract(&a, "lumen-v0-x64.zip", &dest).unwrap();
    assert_eq!(names(&files), ["README", "lumen.exe"]);
    assert_eq!(
        std::fs::read_to_string(dest.join("lumen.exe")).unwrap(),
        "cli"
    );
}

#[test]
fn a_deb_gives_its_three_sidecars_and_nothing_else() {
    let t = tempfile::tempdir().unwrap();
    let deb = deb_fixture(t.path());
    let dest = t.path().join("out");
    let files = extract(&deb, "lumen_0.0.0_amd64.deb", &dest).unwrap();
    assert_eq!(names(&files), ["lumen-daemon", "lumen-mcp", "lumen-tok"]);
    assert_eq!(
        std::fs::read_to_string(dest.join("lumen-mcp")).unwrap(),
        "not a binary: lumen-mcp\n"
    );
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 3);
}

#[test]
fn a_deb_missing_a_sidecar_is_an_error_not_a_short_list() {
    let t = tempfile::tempdir().unwrap();
    let tree = t.path().join("tree");
    sidecar_tree(&tree, "usr/bin");
    std::fs::remove_file(tree.join("usr/bin/lumen-tok")).unwrap();
    let data = t.path().join("data.tar.gz");
    tar_gz(&tree, &data);
    let deb = t.path().join("x.deb");
    std::fs::write(
        &deb,
        ar_bytes(&[
            ("debian-binary", b"2.0\n"),
            ("data.tar.gz", &std::fs::read(&data).unwrap()),
        ]),
    )
    .unwrap();
    let err = extract(&deb, "x.deb", &t.path().join("out")).unwrap_err();
    assert!(err.contains("lumen-tok"), "{err}");
}

#[test]
fn ar_parsing_handles_odd_sizes_and_rejects_damage() {
    let ar = ar_bytes(&[("a", b"abc"), ("b", b"de")]);
    let m = ar_members(&ar).unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!((m[0].0.as_str(), m[0].1), ("a", &b"abc"[..]));
    assert_eq!((m[1].0.as_str(), m[1].1), ("b", &b"de"[..]));
    assert!(ar_members(b"not an archive").is_err());
    assert!(
        ar_members(&ar[..ar.len() - 3]).is_err(),
        "a cut-short member"
    );
}

#[test]
fn the_extraction_destination_must_be_empty() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a.zip");
    std::fs::write(&a, zip_bytes(&[("f", b"x")])).unwrap();
    let dest = t.path().join("out");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("old"), "x").unwrap();
    assert!(extract(&a, "a.zip", &dest).is_err());
    assert!(extract(&a, "a.unknown", &t.path().join("o2")).is_err());
}

#[test]
fn a_setup_exe_without_7z_is_a_clear_na_and_is_never_run() {
    let t = tempfile::tempdir().unwrap();
    let exe = t.path().join("Lumen_0_x64-setup.exe");
    // If anything ran this file it would fail loudly; it is not an executable.
    std::fs::write(&exe, "MZ not really").unwrap();
    let err = assets::extract_nsis_with(&exe, &t.path().join("out"), &["no-such-7z-tool-xyz"])
        .unwrap_err();
    assert!(
        err.starts_with("n/a") && err.contains("7z not found"),
        "{err}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_dmg_gives_its_three_sidecars_and_is_detached_afterwards() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("vol");
    sidecar_tree(&src, "Lumen.app/Contents/MacOS");
    let dmg = t.path().join("Lumen_0.0.0_aarch64.dmg");
    let st = Command::new("hdiutil")
        .args([
            "create",
            "-quiet",
            "-fs",
            "HFS+",
            "-volname",
            "LumenFixture",
            "-srcfolder",
        ])
        .arg(&src)
        .arg(&dmg)
        .status()
        .unwrap();
    assert!(st.success(), "hdiutil create");
    let dest = t.path().join("out");
    let files = extract(&dmg, "Lumen_0.0.0_aarch64.dmg", &dest).unwrap();
    assert_eq!(names(&files), ["lumen-daemon", "lumen-mcp", "lumen-tok"]);
    assert_eq!(
        std::fs::read_to_string(dest.join("lumen-tok")).unwrap(),
        "not a binary: lumen-tok\n"
    );
    let info = Command::new("hdiutil").arg("info").output().unwrap();
    assert!(
        !String::from_utf8_lossy(&info.stdout).contains("LumenFixture"),
        "the image must be detached"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_dmg_off_macos_is_na() {
    let t = tempfile::tempdir().unwrap();
    let dmg = t.path().join("a.dmg");
    std::fs::write(&dmg, "x").unwrap();
    let err = extract(&dmg, "a.dmg", &t.path().join("out")).unwrap_err();
    assert!(err.starts_with("n/a"), "{err}");
}
