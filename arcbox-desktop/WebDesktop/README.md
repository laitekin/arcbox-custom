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
  ⌘C/⌘X copies/cuts the remote selection to the Mac. ⌘V waits for the transfer
  before sending paste. The toolbar provides the same actions.
- Enable **终端快捷键** in Linux terminals: ⌘C/V maps to Ctrl+Shift+C/V.
  Otherwise these map to Ctrl+C/V. ⌘A/Z/Shift+Z/S/F and Mac arrow navigation work.
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

## Access and lifetime

Only 127.0.0.1 is bound. Per-session random tokens stay in URL fragments and are
sent via Authorization/WebSocket subprotocol headers. HTTP checks Host/Origin;
WebSockets require exact Origin and token. No CDN or third-party scripts.
Do not share session URLs. This is local access, not LAN/public access.
Sessions expire after 30 minutes without API activity. Disconnect, stopping or
removing a machine, and changing forwarded ports revoke its sessions. Transfers,
buffers, connections and requests have limits. VNC passwords are not persisted.

## Verification

Run node --test arcbox-desktop/WebDesktop/tests/input.test.mjs.
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
