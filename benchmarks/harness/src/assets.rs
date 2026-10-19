//! Release assets: download one by name for a tag, check it against the sha256 the manifest pins,
//! keep it in the cache, and unpack what a benchmark needs from it.
//!
//! Rules that hold throughout:
//! - A hash that does not match refuses, loudly, and the bad file is not kept.
//! - The cache is never inside the repository, and nothing here writes inside it.
//! - An installer is unpacked, never run. A `.dmg` is mounted read-only, a `.deb` is read as an
//!   `ar` archive, a Windows `setup.exe` is opened with 7-Zip.
//! - A result records an asset as its name and sha256 ([`provenance`]), never a local path.
//!
//! Extraction uses the machine's own tools (`tar`, `unzip`, `hdiutil`, `7z`), which add no crate
//! to the harness: `tar` also reads `.xz`/`.zst` members of a `.deb`, and `ar` is parsed here.

use crate::util::{Ctx, Res, sha256_hex};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The sidecars of the Lumen app, the files the version benchmark measures.
pub const SIDECARS: [&str; 3] = ["lumen-mcp", "lumen-daemon", "lumen-tok"];

/// Where an asset is published: `owner/name` on GitHub and a tag.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub repo: &'a str,
    pub tag: &'a str,
}

/// A verified asset in the cache.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub name: String,
    pub sha256: String,
    /// Where it is on this machine. Never written to a result: use [`provenance`].
    pub path: PathBuf,
}

/// What a result says about an asset: its name and sha256, never where it was.
pub fn provenance(name: &str, sha256: &str) -> String {
    format!("{name} (sha256 {sha256})")
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A bare file name: no separator, no `..`, so a manifest entry cannot name a path.
fn is_plain_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}

/// Refuse a cache directory inside `repo`. Both are resolved through their nearest existing
/// ancestor, so a symlink or a `..` cannot hide it, and the cache need not exist yet.
pub fn ensure_outside_repo(cache: &Path, repo: &Path) -> Res<()> {
    let c = resolve(cache)?;
    let r = resolve(repo)?;
    if c.starts_with(&r) {
        return Err(format!(
            "the cache {} is inside the repository {}; assets must stay out of it",
            cache.display(),
            repo.display()
        ));
    }
    Ok(())
}

fn resolve(p: &Path) -> Res<PathBuf> {
    let abs = std::path::absolute(p).ctx(&format!("resolve {}", p.display()))?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for t in tail.iter().rev() {
                out.push(t);
            }
            // Lexical `..` left in the unresolved tail would defeat the check.
            let mut clean = PathBuf::new();
            for comp in out.components() {
                match comp {
                    std::path::Component::ParentDir => {
                        clean.pop();
                    }
                    std::path::Component::CurDir => {}
                    c => clean.push(c),
                }
            }
            return Ok(clean);
        }
        match (cur.file_name(), cur.parent()) {
            (Some(n), Some(parent)) => {
                tail.push(n.to_os_string());
                cur = parent;
            }
            _ => return Err(format!("cannot resolve {}", p.display())),
        }
    }
}

/// A way to put the bytes of `<repo> <tag> <name>` at `dest`. The default is [`download`]; a test
/// passes one that never touches the network.
pub type Downloader<'a> = &'a dyn Fn(Source<'_>, &str, &Path) -> Res<()>;

/// Fetch `name` of `src` into `cache/assets/<sha256>/<name>`, checking it against `sha256`.
/// A file already there is checked again, not trusted.
pub fn fetch(
    cache: &Path,
    repo_root: &Path,
    src: Source<'_>,
    name: &str,
    sha256: &str,
) -> Res<Fetched> {
    fetch_with(cache, repo_root, src, name, sha256, &download)
}

pub fn fetch_with(
    cache: &Path,
    repo_root: &Path,
    src: Source<'_>,
    name: &str,
    sha256: &str,
    dl: Downloader<'_>,
) -> Res<Fetched> {
    ensure_outside_repo(cache, repo_root)?;
    if !is_plain_name(name) {
        return Err(format!("{name:?} is not a plain asset file name"));
    }
    if !is_sha256(sha256) {
        return Err(format!(
            "the pin for {name} is {sha256:?}, not a 64-hex sha256: an asset is never fetched unpinned"
        ));
    }
    let dir = cache.join("assets").join(sha256);
    let path = dir.join(name);
    if path.is_file() {
        let have = hash_file(&path)?;
        if have == sha256 {
            return Ok(Fetched {
                name: name.to_string(),
                sha256: have,
                path,
            });
        }
        // A cached file that does not match its own directory is damage, not a download.
        std::fs::remove_file(&path).ctx(&format!("remove the damaged cache file {name}"))?;
    }
    std::fs::create_dir_all(&dir).ctx(&format!("create {}", dir.display()))?;
    let part = dir.join(format!("{name}.part"));
    let _ = std::fs::remove_file(&part);
    dl(src, name, &part)?;
    let got = hash_file(&part).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    if got != sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "REFUSED: {name} of {} {} has sha256 {got}, the manifest pins {sha256}; the file was discarded",
            src.repo, src.tag
        ));
    }
    std::fs::rename(&part, &path).ctx(&format!("keep {name} in the cache"))?;
    Ok(Fetched {
        name: name.to_string(),
        sha256: got,
        path,
    })
}

fn hash_file(p: &Path) -> Res<String> {
    let bytes = std::fs::read(p).ctx(&format!("read {}", p.display()))?;
    Ok(sha256_hex(&bytes))
}

/// The arguments of the `curl` fallback: HTTPS only, and a redirect may not leave HTTPS either
/// (`--proto` alone covers the first request, not where it is sent next).
pub fn curl_args(url: &str, dest: &Path) -> Vec<std::ffi::OsString> {
    let mut v: Vec<std::ffi::OsString> = [
        "-fsSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--retry",
        "2",
        "-o",
    ]
    .iter()
    .map(Into::into)
    .collect();
    v.push(dest.into());
    v.push(url.into());
    v
}

/// The default [`Downloader`]: `gh release download` when `gh` runs, else plain HTTPS with `curl`.
pub fn download(src: Source<'_>, name: &str, dest: &Path) -> Res<()> {
    let dir = dest.parent().ok_or("a download needs a directory")?;
    let tmp = tempfile::tempdir_in(dir).ctx("a scratch directory for the download")?;
    let gh = Command::new("gh")
        .args([
            "release", "download", src.tag, "-R", src.repo, "-p", name, "-D",
        ])
        .arg(tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let got = tmp.path().join(name);
    let mut why = String::new();
    match gh {
        Ok(o) if o.status.success() && got.is_file() => {
            return std::fs::rename(&got, dest).ctx("move the download");
        }
        Ok(o) => why = format!("gh: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => why.push_str(&format!("gh: {e}")),
    }
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        src.repo, src.tag, name
    );
    let out = Command::new("curl")
        .args(curl_args(&url, &got))
        .stdin(Stdio::null())
        .output()
        .ctx("run curl")?;
    if !out.status.success() || !got.is_file() {
        return Err(format!(
            "could not download {name} of {} {} ({why}; curl {}: {})",
            src.repo,
            src.tag,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    std::fs::rename(&got, dest).ctx("move the download")
}

// --- extraction ------------------------------------------------------------------------------

fn exe_name(base: &str, windows: bool) -> String {
    if windows {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Unpack what the benchmarks need from the asset at `asset` (called `name`) into `dest`, which
/// must be empty or absent. For an app package (`.dmg`, `.deb`, `setup.exe`) that is the three
/// sidecars, and all three must be there; for a CLI archive (`.tar.gz`, `.tgz`, `.zip`) it is the
/// whole archive. Returns the paths written. Nothing is run.
pub fn extract(asset: &Path, name: &str, dest: &Path) -> Res<Vec<PathBuf>> {
    let lower = name.to_ascii_lowercase();
    if dest.exists()
        && std::fs::read_dir(dest)
            .ctx("read the destination")?
            .next()
            .is_some()
    {
        return Err(format!("{} is not empty", dest.display()));
    }
    std::fs::create_dir_all(dest).ctx(&format!("create {}", dest.display()))?;
    if lower.ends_with(".dmg") {
        extract_dmg(asset, dest)
    } else if lower.ends_with(".deb") {
        extract_deb(asset, dest)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        untar(asset, dest)?;
        list_files(dest)
    } else if lower.ends_with(".zip") {
        unzip(asset, dest)?;
        list_files(dest)
    } else if lower.ends_with("setup.exe") {
        extract_nsis(asset, dest)
    } else {
        Err(format!("do not know how to unpack {name}"))
    }
}

/// Every file under `dir`, sorted. A symbolic link anywhere is refused, not followed: an
/// archive's own links are not trusted, and nothing here reads through one.
fn list_files(dir: &Path) -> Res<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ctx(&format!("read {}", d.display()))? {
            let e = e.ctx("read a directory entry")?;
            let p = e.path();
            // `DirEntry::file_type` does not follow links.
            let ft = e.file_type().ctx(&format!("stat {}", p.display()))?;
            if ft.is_symlink() {
                return Err(refused_link(&p, dir));
            }
            if ft.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn refused_link(link: &Path, root: &Path) -> String {
    let rel = link.strip_prefix(root).unwrap_or(link);
    format!(
        "REFUSED: the archive holds a symbolic link ({}); links in an archive are never followed",
        rel.display()
    )
}

fn run(cmd: &mut Command, what: &str) -> Res<()> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .ctx(&format!("cannot run {what}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn untar(archive: &Path, dest: &Path) -> Res<()> {
    run(
        Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(dest),
        "tar -x",
    )
}

fn unzip(archive: &Path, dest: &Path) -> Res<()> {
    // `unzip` where there is one; otherwise bsdtar (macOS, Windows) reads a zip itself.
    let first = Command::new("unzip")
        .arg("-q")
        .arg(archive)
        .arg("-d")
        .arg(dest)
        .stdin(Stdio::null())
        .output();
    match first {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "unzip exited with {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(_) => untar(archive, dest),
    }
}

/// Copy the three sidecars from `from`, a directory inside `root`, into `dest`. Nothing is
/// followed through a symbolic link: no component of `from` below `root` may be one, a sidecar
/// must be a regular file and not a link, and the resolved file must still be inside `root`.
fn copy_sidecars(root: &Path, from: &Path, dest: &Path, windows: bool) -> Res<Vec<PathBuf>> {
    let rel = from
        .strip_prefix(root)
        .map_err(|_| format!("{} is not inside {}", from.display(), root.display()))?;
    let mut at = root.to_path_buf();
    for c in rel.components() {
        at.push(c);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(refused_link(&at, root));
        }
    }
    let real_root = root.canonicalize().ctx("resolve the archive root")?;
    let mut out = Vec::new();
    for s in SIDECARS {
        let file = exe_name(s, windows);
        let src = from.join(&file);
        let meta = std::fs::symlink_metadata(&src)
            .map_err(|_| format!("the package has no {file} in {}", rel.display()))?;
        if meta.file_type().is_symlink() {
            return Err(refused_link(&src, root));
        }
        if !meta.is_file() {
            return Err(format!("the package has no {file} in {}", rel.display()));
        }
        let real = src.canonicalize().ctx(&format!("resolve {file}"))?;
        if !real.starts_with(&real_root) {
            return Err(refused_link(&src, root));
        }
        let to = dest.join(&file);
        std::fs::copy(&real, &to).ctx(&format!("copy {file}"))?;
        out.push(to);
    }
    Ok(out)
}

/// A mounted disk image, detached when this goes out of scope, panic or not.
pub struct MountGuard {
    mount: PathBuf,
    attached: bool,
}

impl MountGuard {
    /// `hdiutil attach` of `asset` read-only at `mount`; never browsable, never verified-and-run.
    pub fn attach(asset: &Path, mount: &Path) -> Res<MountGuard> {
        run(
            Command::new("hdiutil")
                .args([
                    "attach",
                    "-readonly",
                    "-nobrowse",
                    "-noverify",
                    "-mountpoint",
                ])
                .arg(mount)
                .arg(asset),
            "hdiutil attach",
        )?;
        Ok(MountGuard {
            mount: mount.to_path_buf(),
            attached: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.mount
    }

    /// Detach now and say how it went; a second call, and the drop after it, do nothing.
    pub fn detach(&mut self) -> Res<()> {
        if !self.attached {
            return Ok(());
        }
        self.attached = false;
        // A volume the system is still looking at (indexing, a busy machine) refuses a plain
        // detach for a moment: ask politely a few times before forcing.
        let mut polite = Err(String::new());
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            polite = run(
                Command::new("hdiutil")
                    .args(["detach", "-quiet"])
                    .arg(&self.mount),
                "hdiutil detach",
            );
            if polite.is_ok() {
                break;
            }
        }
        polite.or_else(|_| {
            run(
                Command::new("hdiutil")
                    .args(["detach", "-force", "-quiet"])
                    .arg(&self.mount),
                "hdiutil detach -force",
            )
        })
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}

fn extract_dmg(asset: &Path, dest: &Path) -> Res<Vec<PathBuf>> {
    if !cfg!(target_os = "macos") {
        return Err("n/a: a .dmg can only be opened on macOS (hdiutil)".into());
    }
    let mount = tempfile::tempdir().ctx("a mount point")?;
    // Detached on every exit, a panic included.
    let mut vol = MountGuard::attach(asset, mount.path())?;
    let copied = (|| {
        // The app is the one `*.app` on the volume; the sidecars sit in Contents/MacOS.
        let app = std::fs::read_dir(vol.path())
            .ctx("read the mounted volume")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .ok_or("the .dmg holds no .app")?;
        copy_sidecars(vol.path(), &app.join("Contents").join("MacOS"), dest, false)
    })();
    let detach = vol.detach();
    let files = copied?;
    detach?;
    Ok(files)
}

/// The members of an `ar` archive, in order: `(name, data)`. Only the common format is read
/// (a `.deb`'s three short-named members).
pub fn ar_members(bytes: &[u8]) -> Res<Vec<(String, &[u8])>> {
    const MAGIC: &[u8] = b"!<arch>\n";
    if !bytes.starts_with(MAGIC) {
        return Err("not an ar archive (no `!<arch>` header)".into());
    }
    let mut out = Vec::new();
    let mut at = MAGIC.len();
    while at < bytes.len() {
        let hdr = bytes
            .get(at..at + 60)
            .ok_or("an ar member header is cut short")?;
        if &hdr[58..60] != b"`\n" {
            return Err("an ar member header has a bad terminator".into());
        }
        let name = String::from_utf8_lossy(&hdr[..16]).trim().to_string();
        let name = name.strip_suffix('/').unwrap_or(&name).to_string();
        let size: usize = String::from_utf8_lossy(&hdr[48..58])
            .trim()
            .parse()
            .map_err(|e| format!("an ar member size: {e}"))?;
        let start = at + 60;
        let end = start
            .checked_add(size)
            .filter(|e| *e <= bytes.len())
            .ok_or("an ar member runs past the end of the file")?;
        out.push((name, &bytes[start..end]));
        at = end + (size % 2);
    }
    Ok(out)
}

fn extract_deb(asset: &Path, dest: &Path) -> Res<Vec<PathBuf>> {
    let bytes = std::fs::read(asset).ctx(&format!("read {}", asset.display()))?;
    let members = ar_members(&bytes)?;
    let (data_name, data) = members
        .iter()
        .find(|(n, _)| n.starts_with("data.tar"))
        .ok_or("the .deb has no data.tar member")?;
    let scratch = tempfile::tempdir().ctx("a scratch directory")?;
    let tarball = scratch.path().join(data_name);
    std::fs::write(&tarball, data).ctx("write the data member")?;
    let tree = scratch.path().join("tree");
    std::fs::create_dir(&tree).ctx("create the tree directory")?;
    untar(&tarball, &tree)?;
    copy_sidecars(&tree, &tree.join("usr").join("bin"), dest, false)
}

/// Candidates for 7-Zip, in the order they are tried.
pub const SEVEN_ZIP: [&str; 3] = ["7z", "7zz", "7za"];

/// The first of `candidates` that runs.
pub fn find_tool(candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|c| {
            Command::new(c)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
        .map(|c| c.to_string())
}

fn extract_nsis(asset: &Path, dest: &Path) -> Res<Vec<PathBuf>> {
    extract_nsis_with(asset, dest, &SEVEN_ZIP)
}

pub fn extract_nsis_with(asset: &Path, dest: &Path, tools: &[&str]) -> Res<Vec<PathBuf>> {
    let Some(sz) = find_tool(tools) else {
        return Err("n/a — 7z not found (install 7-Zip to unpack a Windows setup.exe; the installer is never run)".into());
    };
    let scratch = tempfile::tempdir().ctx("a scratch directory")?;
    run(
        Command::new(&sz)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", scratch.path().display()))
            .arg(asset),
        "7z x",
    )?;
    // Where the installer keeps its payload varies, so look for the files by name.
    let all = list_files(scratch.path())?;
    let mut out = Vec::new();
    for s in SIDECARS {
        let file = exe_name(s, true);
        let found = all
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&file)))
            .ok_or_else(|| format!("the installer holds no {file}"))?;
        let to = dest.join(&file);
        std::fs::copy(found, &to).ctx(&format!("copy {file}"))?;
        out.push(to);
    }
    Ok(out)
}

/// Fetch the asset the manifest pins for `version` on `platform` and unpack its sidecars into
/// `dest`. `Ok(Err(reason))` is the manifest's own `na`.
pub fn sidecars_for(
    m: &crate::manifest::VersionsManifest,
    version: &str,
    platform: &str,
    cache: &Path,
    repo_root: &Path,
    dest: &Path,
) -> Res<Result<(Fetched, Vec<PathBuf>), String>> {
    let entry = m.get(version)?;
    let asset = match m.asset(version, platform)? {
        Ok(a) => a,
        Err(reason) => return Ok(Err(reason.to_string())),
    };
    let repo = m
        .meta
        .repo
        .strip_prefix("https://github.com/")
        .ok_or("meta.repo is not a github.com URL")?;
    let f = fetch(
        cache,
        repo_root,
        Source {
            repo,
            tag: &entry.tag,
        },
        &asset.name,
        &asset.sha256,
    )?;
    let files = extract(&f.path, &f.name, dest)?;
    Ok(Ok((f, files)))
}
