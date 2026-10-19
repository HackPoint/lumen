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
        !String::from_utf8_lossy(&info.stdout).contains("Lumen_0.0.0_aarch64.dmg"),
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

// --- hostile archives: nothing escapes, nothing is followed ----------------------------------

/// A ustar archive written by hand, so it can hold what `tar -c` will not make: `..`, absolute
/// names, a link pointing out. `kind` is the type flag: b'0' file, b'2' symlink (the `data` is
/// then the link target).
fn tar_bytes(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, kind, data) in entries {
        let mut h = [0u8; 512];
        let put = |h: &mut [u8; 512], at: usize, s: &[u8]| h[at..at + s.len()].copy_from_slice(s);
        put(&mut h, 0, name.as_bytes());
        put(&mut h, 100, b"0000644\0");
        put(&mut h, 108, b"0000000\0");
        put(&mut h, 116, b"0000000\0");
        let size = if *kind == b'0' { data.len() } else { 0 };
        put(&mut h, 124, format!("{size:011o}\0").as_bytes());
        put(&mut h, 136, b"00000000000\0");
        put(&mut h, 148, b"        ");
        h[156] = *kind;
        if *kind == b'2' {
            put(&mut h, 157, data);
        }
        put(&mut h, 257, b"ustar\0");
        put(&mut h, 263, b"00");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
        out.extend(h);
        if *kind == b'0' {
            out.extend(*data);
            out.extend(vec![0u8; (512 - data.len() % 512) % 512]);
        }
    }
    out.extend(vec![0u8; 1024]);
    out
}

/// What a hostile archive must leave behind: no file outside `dest`, and the planted outside
/// file unread and unchanged.
fn assert_nothing_escaped(t: &Path, dest: &Path) {
    assert!(!t.join("pwned-dotdot").exists(), "`../` escaped");
    assert!(!dest.parent().unwrap().join("pwned-dotdot").exists());
    assert!(
        !t.join("abs").join("pwned-abs").exists(),
        "an absolute name escaped"
    );
    assert_eq!(
        std::fs::read_to_string(t.join("outside").join("secret")).unwrap(),
        "secret\n"
    );
}

fn outside(t: &Path) -> PathBuf {
    let o = t.join("outside");
    std::fs::create_dir_all(&o).unwrap();
    std::fs::write(o.join("secret"), "secret\n").unwrap();
    o
}

#[test]
fn a_tar_with_dotdot_and_absolute_names_writes_nothing_outside() {
    let t = tempfile::tempdir().unwrap();
    outside(t.path());
    let abs = t.path().join("abs").join("pwned-abs");
    let a = t.path().join("hostile.tgz");
    std::fs::write(
        &a,
        tar_bytes(&[
            ("ok/lumen", b'0', b"cli"),
            ("../pwned-dotdot", b'0', b"x"),
            ("../../pwned-dotdot", b'0', b"x"),
            (abs.to_str().unwrap(), b'0', b"x"),
        ]),
    )
    .unwrap();
    let dest = t.path().join("dest").join("out");
    // Whether the machine's tar skips the bad members or stops, they are not written.
    let _ = extract(&a, "hostile.tgz", &dest);
    assert_nothing_escaped(t.path(), &dest);
}

#[test]
fn a_tar_with_an_escaping_symlink_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let out = outside(t.path());
    let a = t.path().join("hostile.tgz");
    std::fs::write(
        &a,
        tar_bytes(&[
            ("lumen", b'0', b"cli"),
            ("link", b'2', out.to_str().unwrap().as_bytes()),
            ("file-link", b'2', b"../../outside/secret"),
        ]),
    )
    .unwrap();
    let dest = t.path().join("dest").join("out");
    let err = extract(&a, "hostile.tgz", &dest).unwrap_err();
    assert!(
        err.contains("REFUSED") && err.contains("symbolic link"),
        "{err}"
    );
    assert!(
        !err.contains(t.path().to_str().unwrap()),
        "no scratch path in {err}"
    );
    assert_nothing_escaped(t.path(), &dest);
}

/// A zip whose central header says its first entry is a unix symlink.
fn zip_with_symlink(name: &str, target: &str) -> Vec<u8> {
    let mut z = zip_bytes(&[(name, target.as_bytes())]);
    let at = z
        .windows(4)
        .position(|w| w == 0x0201_4b50u32.to_le_bytes())
        .unwrap();
    z[at + 4] = 20;
    z[at + 5] = 3; // made by: unix
    z[at + 38..at + 42].copy_from_slice(&(0o120_777u32 << 16).to_le_bytes());
    z
}

#[test]
fn a_zip_with_dotdot_absolute_and_symlink_entries_writes_nothing_outside() {
    let t = tempfile::tempdir().unwrap();
    let out = outside(t.path());
    let abs = t.path().join("abs").join("pwned-abs");
    let a = t.path().join("hostile.zip");
    std::fs::write(
        &a,
        zip_bytes(&[
            ("lumen.exe", b"cli"),
            ("../pwned-dotdot", b"x"),
            ("../../pwned-dotdot", b"x"),
            (abs.to_str().unwrap(), b"x"),
        ]),
    )
    .unwrap();
    let dest = t.path().join("dest").join("out");
    let _ = extract(&a, "hostile.zip", &dest);
    assert_nothing_escaped(t.path(), &dest);

    let b = t.path().join("link.zip");
    std::fs::write(&b, zip_with_symlink("link", out.to_str().unwrap())).unwrap();
    let dest2 = t.path().join("dest2");
    // Info-ZIP makes the link, bsdtar may write it as a file; either way it is never followed
    // out: a link is refused, a file holds only the target's text.
    match extract(&b, "link.zip", &dest2) {
        Err(e) => assert!(e.contains("REFUSED"), "{e}"),
        Ok(files) => {
            for f in files {
                assert!(
                    !std::fs::symlink_metadata(&f)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
            }
        }
    }
    assert_nothing_escaped(t.path(), &dest2);
}

#[cfg(unix)]
#[test]
fn a_symlinked_sidecar_in_a_deb_is_refused_not_copied() {
    use std::os::unix::fs::symlink;
    let t = tempfile::tempdir().unwrap();
    let out = outside(t.path());
    let tree = t.path().join("tree");
    sidecar_tree(&tree, "usr/bin");
    std::fs::remove_file(tree.join("usr/bin/lumen-mcp")).unwrap();
    symlink(out.join("secret"), tree.join("usr/bin/lumen-mcp")).unwrap();
    let data = t.path().join("data.tar.gz");
    tar_gz(&tree, &data);
    let deb = t.path().join("a.deb");
    std::fs::write(
        &deb,
        ar_bytes(&[
            ("debian-binary", b"2.0\n"),
            ("data.tar.gz", &std::fs::read(&data).unwrap()),
        ]),
    )
    .unwrap();
    let dest = t.path().join("dest").join("out");
    let err = extract(&deb, "a.deb", &dest).unwrap_err();
    assert!(err.contains("REFUSED"), "{err}");
    assert!(
        !dest.join("lumen-mcp").exists(),
        "the target must not be copied"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_usr_bin_in_a_deb_is_refused() {
    use std::os::unix::fs::symlink;
    let t = tempfile::tempdir().unwrap();
    let out = outside(t.path());
    for s in assets::SIDECARS {
        std::fs::write(out.join(s), "outside sidecar").unwrap();
    }
    let tree = t.path().join("tree");
    std::fs::create_dir_all(tree.join("usr")).unwrap();
    symlink(&out, tree.join("usr/bin")).unwrap();
    let data = t.path().join("data.tar.gz");
    tar_gz(&tree, &data);
    let deb = t.path().join("a.deb");
    std::fs::write(
        &deb,
        ar_bytes(&[("data.tar.gz", &std::fs::read(&data).unwrap())]),
    )
    .unwrap();
    let dest = t.path().join("dest").join("out");
    let err = extract(&deb, "a.deb", &dest).unwrap_err();
    assert!(err.contains("REFUSED"), "{err}");
    assert!(!dest.join("lumen-mcp").exists());
}

/// The guard detaches when a panic unwinds through it.
#[cfg(target_os = "macos")]
#[test]
fn a_panic_while_mounted_still_detaches() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("vol");
    sidecar_tree(&src, "Lumen.app/Contents/MacOS");
    let dmg = t.path().join("g.dmg");
    // `hdiutil create` is the part that fails under load (the failure seen was this step's exit
    // status, not the guard): try again a few times, and show what it said if it never works.
    let mut said = String::new();
    let created = (0..5).any(|n| {
        if n > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = std::fs::remove_file(&dmg);
        let out = Command::new("hdiutil")
            .args([
                "create",
                "-quiet",
                "-fs",
                "HFS+",
                "-volname",
                "LumenPanicFixture",
                "-srcfolder",
            ])
            .arg(&src)
            .arg(&dmg)
            .output()
            .unwrap();
        said = String::from_utf8_lossy(&out.stderr).into_owned();
        out.status.success()
    });
    assert!(created, "hdiutil create failed 5 times: {said}");
    let mount = t.path().join("mnt");
    std::fs::create_dir(&mount).unwrap();
    let r = std::panic::catch_unwind(|| {
        let g = assets::MountGuard::attach(&dmg, &mount).unwrap();
        assert!(g.path().join("Lumen.app").exists());
        panic!("boom");
    });
    assert!(r.is_err());
    // `hdiutil info` names the image file, not the volume. The detach has returned by now, but
    // under load the kernel can take a moment to drop the image from the list, so ask for up to
    // 15 s before calling it a leak; an image that is still there after that is a real leak.
    let still_mounted = || {
        let info = Command::new("hdiutil").arg("info").output().unwrap();
        String::from_utf8_lossy(&info.stdout).contains("g.dmg")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while still_mounted() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(!still_mounted(), "a panic left the image mounted");
}

#[test]
fn curl_refuses_a_redirect_off_https() {
    let a: Vec<String> = assets::curl_args("https://x/y", Path::new("/d"))
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let at = |f: &str| a.iter().position(|x| x == f).unwrap();
    assert_eq!(a[at("--proto") + 1], "=https");
    assert_eq!(a[at("--proto-redir") + 1], "=https");
    assert_eq!(a.last().unwrap(), "https://x/y");
}
