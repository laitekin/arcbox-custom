# ArcBox browser desktop

In a running machine's Info → Forwarded ports, use **Open in Browser** on its TCP
VNC forward. ArcBox serves the bundled noVNC client on a random localhost HTTP
port and bridges an authenticated WebSocket to that exact forward. Keep the app
running. A VNC desktop must already be installed. Open VNC remains available.

## Clipboard and input

- UTF-8 text, emoji, and PNG images travel through the guest agent. Mac TIFF
  screenshots are converted to PNG. Limit: 16 MiB. Files, folders and rich HTML
  are not supported.
- The guest needs X11 and xclip: sudo apt install xclip on Ubuntu; apk add xclip
  as root on Alpine. Wayland clipboard sessions are not supported.
- Automatic sync runs once per second while the browser desktop is focused.
  ⌘C/⌘X copies/cuts the remote selection to the Mac. ⌘V pastes the newest
  clipboard, as on the Mac: it transfers the Mac clipboard first only when it changed
  since the last transfer either way, otherwise the machine's own clipboard is pasted
  straight away. Success is silent; ⌘C with nothing selected changes nothing. The
  toolbar provides the same actions.
- **⌘C / ⌘V 发送** (under More) defaults to 自动识别终端: before a copy or paste the
  guest reports the focused window's WM_CLASS (xdotool or xprop; most desktops have
  xprop). Terminals (xterm, GNOME Terminal, Konsole, xfce4-terminal, kitty, Alacritty
  and others) get Ctrl+Shift+C/V, so ⌘C never interrupts the running program and ⌘X
  copies; other apps get Ctrl+C/X/V. Without either tool, auto falls back to Ctrl+C/V;
  pick Ctrl+Shift+C / V by hand in a terminal then. Shifted chords send upper-case
  keysyms; TigerVNC otherwise releases Shift and Ctrl+Shift+V arrives as Ctrl+V.
- Other ⌘ shortcuts map to Ctrl: ⌘A/B/D/E/F/G/I/K/O/P/S/U/Y/Z, ⌘⇧Z and ⌘/.
  ⌘Q/W/N/T/H/M/L/R and ⌘Tab stay with the browser and macOS. ⌘←/→ go to line
  start/end, ⌘↑/↓ to document start/end, ⌥←/→ move by word, ⌥⌫/⌥⌦ delete a word,
  ⌘↩ sends Ctrl+Enter.
- Mac input-source shortcuts remain local. A real textarea handles IME composition,
  forwards committed Unicode, and suppresses duplicate commit events. Click the
  desktop and type; no separate input field is shown. Control+Space,
  Caps Lock, and Globe/Fn follow existing macOS settings. Candidate windows appear
  near the last desktop click (the VNC protocol does not provide the guest text caret).
- X display defaults to VNC port minus 5900 (5901 → :1). Change **X 显示** for a
  different mapping. Helpers run as the display socket owner with its Xauthority.
- Disable automatic sync when unwanted; explicit copy/paste still works.
  Unsupported contents are left alone. Simultaneous changes prioritize the Mac.
  Switching tabs prevents an old background read from updating the Mac clipboard.

## Zoom

The zoom control in the toolbar (−, the percentage menu, +; or ⌘−, ⌘+, ⌘0) picks
how the desktop fits the window. The choice is remembered in the browser.

- 机器分辨率 · 跟随窗口 (default): the machine's resolution follows the window, so
  the desktop is always shown whole, also after the window is resized. Zoom sets
  how large it looks: at 100% one machine pixel is one CSS pixel; at 50% the machine
  gets twice the window's size (sharp on Retina, more room); at 200% half of it.
  When the VNC server refuses resizing, the picture is scaled to fit instead and
  the label reads 适应 N%.
- 机器分辨率 · 保持当前分辨率: the machine keeps its resolution. 适应窗口 scales the
  picture to fit; a percentage shows it at that size and scrolls when larger.
  ⌘0 returns to 适应窗口 (100% in 跟随窗口).

desktop.js reaches into three noVNC 1.7.0 internals for this (`_screenSize`,
`_requestRemoteResize`, `_updateScale`); without them the zoom control is hidden
and the desktop follows the window as before. Recheck them when updating noVNC.

## Access and lifetime

Only 127.0.0.1 is bound. Per-session random tokens stay in URL fragments and are
sent via Authorization/WebSocket subprotocol headers. HTTP checks Host/Origin;
WebSockets require exact Origin and token. No CDN or third-party scripts.
Do not share session URLs. This is local access, not LAN/public access.
Sessions expire after 30 minutes without API activity. Disconnect, stopping or
removing a machine, and changing forwarded ports revoke its sessions. Transfers,
buffers, connections and requests have limits. VNC passwords are not persisted.

## Verification

Run node --test arcbox-desktop/WebDesktop/tests/input.test.mjs arcbox-desktop/WebDesktop/tests/view.test.mjs.
Run make -C arcbox-desktop test with XCODEBUILD_EXTRA=-only-testing:ArcBoxTests/WebDesktopTests.

The opt-in live XCTest needs X11 :1, xclip and a localhost:5901 forward. Pass
TEST_RUNNER_WEB_DESKTOP_TEST_MACHINE through the Makefile XCODE_ENV override.
It restores the previous supported guest clipboard. Optional
TEST_RUNNER_WEB_DESKTOP_BROWSER_URL_FILE writes a temporary session URL and keeps
the session alive for four minutes for browser validation.

The unmodified noVNC v1.7.0 runtime's source commit and MPL-2.0 license are in
vendor/README.arcbox.md and vendor/LICENSE.noVNC.

## Compact desktop

A 36 px toolbar leaves the rest of the viewport to the desktop. Clipboard and X display settings are under More; there is no bottom input bar. Network check runs fixed DNS, Cloudflare HTTPS/exit IP and Google HTTPS probes inside the guest, with individual timeouts. It does not change proxy or DNS settings and does not claim a full leak/fingerprint audit.

Background clipboard RPCs use a separate queue from keyboard input. Explicit copy/paste
still preserves key ordering, including text typed immediately after paste. Losing
browser focus deactivates clipboard ownership even while a read is in flight.

### Local verification — 2026-10-25

- 14 selected XCTest cases passed (WebDesktopTests and MachineResourcesTests).
- 7 Node input regression tests passed, including IME commit deduplication, Mac
  shortcuts, candidate placement and slow clipboard / keyboard ordering.
- A running Alpine X11 guest round-tripped approximately 270 KB of Chinese text
  plus emoji and PNG bytes exactly. Mac named-pasteboard text, PNG and TIFF
  conversion passed. The authenticated WebSocket received the live RFB greeting.
- Guest network probe executed successfully: DNS resolved; Cloudflare and Google
  HTTPS timed out or failed in that guest's network. The diagnostic reports those
  failures rather than treating them as a healthy connection.
- Swift development build and scoped Swift formatting checks passed.
- Connected-browser verification of the final compact layout, native macOS Pinyin
  candidate UI and end-to-end keyboard/clipboard shortcuts remains pending the
  user's VNC login. No VNC password was read, changed or stored.

### Local verification — 2026-10-27 (zoom and Mac clipboard keys)

Run in a Linux container against TigerVNC 1.13 (one resizable display and one
started with -AcceptSetDesktopSize=0), Chromium through Playwright, and a Node
stand-in for WebDesktopServer that runs the real guest helper script from
DesktopClipboard.swift. 17 end-to-end checks passed:

- Follow window at 100/90/67/50%: the machine's resolution is the window divided by
  zoom and the picture always fits, also after resizing the window; ⌘+/⌘−/⌘0 and the
  menu work, and the setting survives a reload.
- Refused resize: the picture is fitted and labelled 适应 67%. Fixed resolution at
  fit, 100% and 150%: the pointer lands on the right machine pixel while scrolled.
- Mousepad (GTK): ⌘V pastes Mac Unicode text, ⌘A ⌘C copies it back, ⌘X cuts, and a
  second ⌘V with an unchanged Mac clipboard pastes without a transfer.
- xfce4-terminal: detected as a terminal; ⌘V pastes into a running `cat`, ⌘C with
  nothing selected does not interrupt it, ⌘C copies a selection to the Mac. xterm
  is detected too.

Not run here: the Swift changes were not compiled (no Swift toolchain in this
environment), and Safari and the real Mac pasteboard were not exercised.
