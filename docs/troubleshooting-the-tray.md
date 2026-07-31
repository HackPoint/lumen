# No menu-bar icon (macOS)

Lumen is a menu-bar app whose windows both start hidden, so a status item that never appears
leaves nothing to click — no popover, no window, and no way to reach the fault reporter to
report the problem. This page is the diagnosis path, ordered so the first step is the one most
likely to end it.

Tracked as [#5](https://github.com/HackPoint/lumen/issues/5).

> **A correction.** Earlier guidance — in the 1.5.1 CHANGELOG and on issue #5 — asked for the
> line `TRAY: build failed:` from `~/Library/Logs/io.speedata.lumen/Lumen.log`. **That file
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
defaults read io.speedata.lumen | grep NSStatusItem
defaults read Lumen | grep NSStatusItem
```

Check both — Lumen has written under both domain names. You are looking for:

```
"NSStatusItem Visible Item-0" = 0;
```

If it is there and `0`, that is the cause. Restore it:

```sh
defaults delete io.speedata.lumen "NSStatusItem Visible Item-0"
defaults delete Lumen "NSStatusItem Visible Item-0"
killall Lumen
open -a Lumen
```

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

The login-item label is `Lumen`, **not** `io.speedata.lumen`. That distinction caused a
separate bug in the same report — the uninstaller was unloading a label that never existed, so
every uninstall left a login item behind trying to launch a deleted app. If you have ever
uninstalled Lumen:

```sh
launchctl bootout gui/$(id -u)/Lumen 2>/dev/null
rm -f ~/Library/LaunchAgents/Lumen.plist
```

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
