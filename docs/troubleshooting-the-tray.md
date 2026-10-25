# No menu-bar icon (macOS)

Lumen is a menu-bar app whose windows both start hidden, so a status item that never appears
leaves nothing to click — no popover, no window, and no way to reach the fault reporter to
report the problem. This page is the diagnosis path, ordered so the first step is the one most
likely to end it.

Tracked as [#5](https://github.com/HackPoint/lumen/issues/5).

> **A correction.** Earlier guidance — in the 1.5.1 CHANGELOG and on issue #5 — asked for the
> line `TRAY: build failed:` from `~/Library/Logs/io.hackpoint.lumen/Lumen.log`. **That file
> does not exist in a released build.** The logger was registered only under
> `cfg!(debug_assertions)`, so in the shipped binary every log call went to a no-op sink. The
> instruction was wrong. Everything below works on an unmodified install.

---

## 1. The likely cause: macOS remembers a hidden status item

Hold ⌘ and you can drag menu-bar icons around — and drag one **off** the bar entirely. When
that happens AppKit writes the removal to the app's preferences, and it is permanent: on every
later launch the status item is created and then immediately hidden. From the outside the app
looks broken. From the inside everything succeeded, which is why nothing was logged.

Check for it:

```sh
defaults read io.hackpoint.lumen | grep NSStatusItem
defaults read Lumen | grep NSStatusItem
```

Check both — Lumen has written under both domain names. You are looking for either of:

```
"NSStatusItem Visible Item-0" = 0;
"NSStatusItem VisibleCC Item-0" = 0;
```

The second is the form current macOS writes, `CC` for Control Center, which manages status items
there. If either is there and `0`, that is the cause. Restore it by deleting the key you found, in
the domain you found it in:

```sh
defaults delete io.hackpoint.lumen "NSStatusItem Visible Item-0"
defaults delete io.hackpoint.lumen "NSStatusItem VisibleCC Item-0"
defaults delete Lumen "NSStatusItem Visible Item-0"
defaults delete Lumen "NSStatusItem VisibleCC Item-0"
killall Lumen
open -a Lumen
```

From 1.6.0 Lumen does this itself on launch and opens its window to say so, and `lumen doctor`
names the key it found.

A `NSStatusItem Preferred Position` key on its own is normal and harmless — that is just where
the icon last sat.

## 2. A menu-bar manager is hiding it

Bartender, Ice, Hidden Bar, Dozer, Vanilla and TopNotch all work by moving status items into
an overflow area.

```sh
ls /Applications | grep -iE 'bartender|ice|hidden|dozer|vanilla|topnotch'
ps -ax | grep -iE 'bartender|ice|hidden' | grep -v grep
```

If one is running, look in its hidden section before assuming Lumen failed.

## 3. The menu bar is full

macOS does not queue status items. If there is no room — common on a 13" display with a notch
and a row of existing icons — the item exists and is simply never drawn. Lumen cannot fix this;
you have to make room.

```sh
system_profiler SPDisplaysDataType | grep -iE 'resolution|notch'
```

## 4. Isolate the launch context

Quit Lumen, then start it by hand:

```sh
/Applications/Lumen.app/Contents/MacOS/Lumen
```

- **Icon appears now** → something about the login-item or Homebrew-postflight launch
  environment is responsible, not the app itself.
- **Icon still missing** → the launch environment is ruled out.

Either way this is worth doing because it is the one way to see the process's own output on a
released build. Leave it running in the terminal and note anything printed.

## 5. The system log, which needs nothing of ours

```sh
log show --predicate 'process == "Lumen"' --last 10m --info --debug
```

## 6. Confirm what is actually running

```sh
ps -ax | grep -i lumen | grep -v grep
launchctl list | grep -i lumen
launchctl print gui/$(id -u)/Lumen
```

The login-item label is `Lumen`, **not** `io.hackpoint.lumen`. That distinction caused a
separate bug in the same report — the uninstaller was unloading a label that never existed, so
every uninstall left a login item behind trying to launch a deleted app. If you have ever
uninstalled Lumen:

```sh
launchctl bootout gui/$(id -u)/Lumen 2>/dev/null
rm -f ~/Library/LaunchAgents/Lumen.plist
```

## The icon is hidden by a full menu bar or the notch

If the icon is crowded out, Lumen is still running. Closing the main window never quits it
(1.10.0): on macOS the window closes and the Dock icon stays, and clicking it opens the window
again; on other systems the window minimizes. From a terminal, `lumen show` opens the window.
Quit from the tray menu, the Dock menu or Cmd-Q.

To get the icon back, make room: hold Cmd and drag the icon to the right, so the icons to its
left are the ones hidden first, or remove icons you do not need.

## Where the backup is

1.10.0 keeps the previous data folder, whole, as `legacy-backup-<date>` inside the new one
(`-2`, `-3` if the name was taken); its files are under `data`.

- macOS: `~/Library/Application Support/io.hackpoint.lumen/legacy-backup-<date>`
- Windows: `%USERPROFILE%\AppData\Roaming\io.hackpoint.lumen\legacy-backup-<date>`
- Linux: `~/.local/share/io.hackpoint.lumen/legacy-backup-<date>`

Until Setup has run, rows an old app wrote late are merged from it at each start. After Setup
the backup is frozen: made read-only and never read for merging again.

## Bringing a ledger in with `lumen import-ledger`

```sh
lumen import-ledger --dry-run "<path>"   # say what would be added, change nothing
lumen import-ledger "<path>"
```

`<path>` is a folder, its `lumen.db`, or a set-aside ledger such as `lumen.db.pre-legacy`.
Rows already in your ledger are not added twice, and an imported folder moves into
`legacy-imports` beside the ledger. Quit any app that uses the folder first.

Use it for a ledger Lumen set aside: someone who installed the 1.9.0 build re-published on
2026-10-20 had a ledger in the new folder, which 1.10.0 sets aside as `lumen.db.pre-legacy`
and says so at start.

## "An older Lumen wrote to ... after the move"

The full text is: "An older Lumen wrote to <folder> after the move. Nothing from it was
merged. Quit it, then run: lumen import-ledger ...". The previous folder was re-created, by an
old app started again or by a restored copy. Lumen never merges it on its own. Quit that app,
then run the command in the message. A related message, "Another Lumen ledger was found in
<folder> and left alone", is handled the same way.

## Deleting a read-only backup

The freeze makes the backup's files read-only, so a plain delete fails. Once Setup has run and
the backup is frozen, nothing Lumen still uses is in it, and deleting loses nothing.

- macOS and Linux:

  ```sh
  chmod -R u+w ~/Library/Application\ Support/io.hackpoint.lumen/legacy-backup-<date>
  rm -rf ~/Library/Application\ Support/io.hackpoint.lumen/legacy-backup-<date>
  ```

  (On Linux the folder is `~/.local/share/io.hackpoint.lumen`.)
- Windows: clear the read-only attribute, then delete.

  ```powershell
  attrib -R "$env:USERPROFILE\AppData\Roaming\io.hackpoint.lumen\legacy-backup-<date>\*" /S /D
  Remove-Item -Recurse -Force "$env:USERPROFILE\AppData\Roaming\io.hackpoint.lumen\legacy-backup-<date>"
  ```

Do not delete it before Setup has run: until then it can still hold rows that are merged in.

---

## Reaching the app while the icon is missing

- **Double-click Lumen in /Applications**, or `open -a Lumen`. On builds after 1.5.1 this
  reveals the main window; on 1.5.1 and earlier it does nothing.
- **From the terminal**, the CLI inside the bundle works even when the GUI is unreachable:

  ```sh
  /Applications/Lumen.app/Contents/MacOS/lumen-cli report --dry-run
  ```

  That renders a fault report — including this problem — without needing the window. Drop
  `--dry-run` and add `--yes` to file it.

## If none of the above explains it

Please add to [#5](https://github.com/HackPoint/lumen/issues/5): the output of step 1 (both
domains), whether step 4 changed anything, your `sw_vers`, and a screenshot of your menu bar.
Step 1's output is the single most useful thing — every hypothesis predicts a different answer
there.
