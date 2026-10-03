# Changelog

This file feeds the GitHub Release notes. Keep entries user-facing: describe what
changed for someone *using* MyKVM, not the internal/CI plumbing. The release
workflow publishes whatever is under `## [Unreleased]`, so move those entries
under a version heading when you cut a release (or just leave them — the next
release will reuse them).

## [Unreleased]

### Added

- Clipboard history is saved on this machine and survives restarts; the popup hotkey is now configurable in Settings (with a "clear history" button in the popup itself).
- Transfer history: the last 50 finished transfers (sent and received) are listed on the Devices tab, with one-click resend for failed sends. The list is persisted and clearable.
- Interrupted multi-file sends can be resumed: after a restart, a banner offers to send the files that never finished (the per-file resume from the previous release picks up partial files where they stopped).
- Clipboard sync is event-driven on Windows: a copy now reaches the other machine within milliseconds of Ctrl+C, instead of up to 150 ms later on the next poll.
- macOS: the fullscreen crossing guard now works on macOS too (it was Windows-only) — a fullscreen app or video pauses edge crossing so games are never interrupted.
- When the paired-controller whitelist hits its cap of 8, the least recently used pair is evicted instead of merely the oldest one; actively used pairs are never dropped.
- Fullscreen guard: while a fullscreen app (game/video) is foreground on this machine, edge crossing is paused so a stray mouse push never yanks you out mid-game. On by default; toggle in Settings.
- File clipboard: copy files with Ctrl+C on one machine and press Ctrl+V on the other to paste them. Received files land in Downloads\MyKVM Transfers\Clipboard and the local clipboard points at them automatically. Total size per copy is capped at 24 MB. Requires both sides on this version or newer.
- Clipboard history: the last 20 synced clipboards (text and file lists, ~64 MB budget) are kept per machine. Press Ctrl+Shift+V anywhere to open a picker and restore an earlier entry to the local clipboard, which syncs it to the other side like any other copy.
- File transfers can be cancelled: the progress toast gets a Cancel button that stops the sender right away instead of waiting for the file to finish.
- Interrupted file transfers resume: if a transfer is cut off (sleep, app restart, network drop), the receiver keeps the partial file and the next attempt of the same file (same name and size) continues from where it stopped instead of restarting. The finish-time SHA-256 still verifies the complete file.
- Edge-dragged files between two Windows machines now land in Downloads\MyKVM Transfers instead of a hidden staging folder.

### Fixed

- Startup wiring (discovery signing key, file-clipboard landing directory) could be silently skipped when a second `.setup()` registration replaced the first — received file-clipboard payloads then failed with "landing directory is not configured". Both wirings now live in the single setup hook and are logged at startup.
- Keyboard shortcuts pressed while typing fast no longer break: key events that ride the same datagram are now tracked on the controlled machine's held-state ledger, so an IME chord like Alt+Q (WeChat voice input) is no longer released the instant it is injected.
- After a restart the two machines sync their key/mouse readiness right away: a peer coming online refreshes the controlling machine's target list immediately instead of waiting for the next announce cycle, so crossing works without clicking around or re-pairing. Re-pairing a machine that is already auto-paired no longer waits for a confirmation code that never comes ("no pairing challenge received").
- Peer mode: screens of a newly paired machine are now placed to the right of the local screens instead of on top of them, so both directions of crossing work right after pairing instead of only one.
- Corner guard fixes: changing the guard size in Settings no longer freezes the spinner or restarts input on both machines, and the guard now also protects the return path (B → A) instead of only the forward one.
- Deliberately skipped low-level hook events no longer look like a dead hook, so the input hooks are no longer torn down and reinstalled dozens of times during a normal remote session.

- Open pairing: devices discovered on the same LAN are paired automatically — no confirmation code. Trust is anchored on each device's certificate (whitelist-first authorization, the shared pair secret stays as a legacy fallback); a new "LAN Auto-Pairing" toggle in Settings switches back to the confirmation-code flow, and the cap on paired controllers is 8.
- Wake-on-LAN: devices now advertise their NIC MAC, and each device in the Devices tab gets a Wake button that wakes a sleeping machine.
- Optional screen lock on leave (off by default): when you slide control onto another machine, this machine locks itself — privacy for shared spaces.
- Discovery packets are now signed (HMAC-SHA256 bound to the sender's identity), so a forged announce claiming a trusted device is dropped. Unsigned packets from older peers are still accepted.
- Keyboard batching: key events that queue while a datagram is in flight ride the next one, cutting packet rate for fast typists and auto-repeat without added latency (batches flush within 12 ms).

- Corner guard: the cursor no longer crosses to another machine when a push starts inside a dead zone around the local screen's four corners — clicking a window's close button or the Start menu can no longer throw your cursor onto the other screen. On by default (32 px zones); adjust the zone size or turn it off in Settings. Sliding along the edge out of the corner crosses normally, and screen-switch hotkeys are unaffected.
- Peer mode: two machines can control each other. Pick "Peer" during setup (or switch work modes in Settings), pair the two machines once, and each side can slide its mouse and keyboard onto the other's screens — and receive the other's input too. While one side is driving, local input stays local and any in-flight control session hands over, so the two directions never fight. Screen-switch hotkeys and the lock-screen input service work in peer mode as well. Requires both sides on this version or newer.
- Manually added peers keep their selected connection IP, including after pairing, rediscovery, and address changes (#26).
- Drag-and-drop files across machines (ShareMouse-style, experimental): drag files on the machine that owns the keyboard and mouse onto a controlled machine. Controlling Windows → Mac: drag files toward the screen edge that borders the Mac — a document icon follows the cursor onto the Mac, and releasing over an open Finder folder drops the files there (otherwise the Desktop). Controlling Mac → Windows client is also in. Requires file transfer to be enabled in Settings, and both sides on this version or newer.
- Drag files the other way too — from a controlled machine back to the controller. While controlling a Mac from Windows, grab a file on the Mac and drag it back across the edge onto Windows: it becomes a real native drag on Windows that you can drop into any folder, app, or field. Requires both sides on this version or newer.
- Fetch the other machine's log from Settings → Diagnostics: a server pulls its online clients' logs ("Fetch Client Log"), a client pulls its server's ("Fetch Server Log"). The log lands in Downloads/MyKVM Remote Logs, so troubleshooting no longer means copying files between machines. Requires both sides on this version or newer.

### Fixed

- One input that fails to send (the connection is being rebuilt, a Wi-Fi hiccup) no longer hands control back to the controlling machine; only inputs that keep failing for a second do. Before, your next keys and shortcuts could land on the controlling machine while you were still looking at the other screen, until you moved the mouse over again.
- Pairing a client that was already paired (the two machines swapped roles, or the server was reinstalled) now shows the pairing code on the client, instead of bringing its window up with no code to type.
- macOS: crossing from the Mac onto another machine hides the Mac cursor right away, instead of leaving it painted at the screen edge for up to a second.
- macOS: with its window closed or minimized, MyKVM leaves the Dock and Cmd+Tab and stays in the menu bar; it comes back when the window is shown.
- A machine running a proxy in TUN mode (Clash, Mihomo, Surge, sing-box) keeps the same device identity whether the proxy is on or off, instead of showing up as a second device.
- Clipboard sync to a machine that stopped answering backs off to one retry a minute and logs once, instead of retrying every 2 seconds and filling the log.

- macOS: memory no longer grows with every image received through clipboard sync. Each synced image leaked its full size, so a few dozen screenshots could push MyKVM to around 2 GB (discussion #32).
- Controlling another machine no longer drops you back to local control when that machine refuses a clipboard sync (for example clipboard sync is off there, or it runs an older version). The keyboard/mouse connection stays up, refused content is not re-sent every 2 seconds, and large clipboard images get enough time to be acknowledged.
- After a Wi-Fi stall the controlled machine no longer replays seconds of stale mouse movement and clicks.
- macOS: pushing the cursor against the bottom of a display no longer jumps into a machine arranged above it (#34).
- macOS: when the controlled machine stops accepting input, clicks and keys now hand control back to the Mac instead of freezing the trackpad and mouse until MyKVM is force-quit. A file drag that an older client refuses is delivered to that machine's Desktop instead (#33).
- The device list and diagnostics show every IPv4 address of this machine, so a direct-cable or Thunderbolt-bridge address is visible for manual pairing, not just the Wi-Fi one (#33).
- Windows: MyKVM starts a stopped lock-screen input service from an older install, so clicks work on the lock screen again (#27).
- macOS: launch at startup opens MyKVM once and silently, instead of racing macOS "Reopen windows when logging back in" and showing the window. Display changes on wake are applied once instead of several times, without blocking the app, and input-capture errors are now written to the log.
- macOS: returning to the Mac after a long session with the window hidden no longer stalls while the hidden cursor is restored.
- Mouse movement keeps a steady 125 Hz instead of dropping to about 62 Hz when input callbacks jitter.
- Windows: precision touchpads and smooth scroll wheels now scroll the controlled machine.
- Windows: a clipboard held open by another app no longer drops a synced copy.
- Updates: a stalled update check gives up after 20 seconds with a clear message, and .deb/.rpm installs are offered their own update package instead of the AppImage.
- Background input: blocking clipboard/file handlers no longer occupy QUIC workers; reconnecting input stays local until the transport is ready and retries with full pairing credentials.
- Clipboard images are encoded once, so a 4K screenshot fits the stream limit. Unchanged clipboard contents use the OS change counter instead of repeatedly reading and encoding the image.
- Clipboard image sync compresses screenshots as PNG on the wire when that is smaller — a 4K screenshot drops from ~33 MB to a few hundred KB per copy. Older peers ignore the new format, so a mixed-version pair just pauses image sync until both sides update.
- File transfers are now resilient and verified: each packet is retried a few times before giving up (one lost acknowledgement no longer aborts a multi-GB transfer), and the receiver checks a SHA-256 of the whole file when it finishes, dropping a corrupt transfer instead of landing a broken file. Checksum verification needs both sides on this version or newer.
- macOS: display changes refresh both saved placement and native input coordinates. Same-resolution displays keep separate placements, and active sharing stays responsive to input while the window is hidden, including receive-only mode.
- Windows input service: installation/repair configures automatic recovery after failures; status files are refreshed at most once per second unless state changes, and new input is skipped if attaching the current desktop fails.
- LAN discovery cannot replace a paired device's transport certificate; an identity change now requires re-pairing.
- Linux reports its unavailable input backend explicitly instead of advertising input readiness.
- Preserve the saved display layout when no displays are temporarily available during sleep or startup, instead of replacing it with a 1×1 placeholder (#30).
- macOS: remote Caps Lock now switches the input source reliably. It switches the source directly (via Carbon TIS) instead of injecting the ⌃Space hotkey, which a Chinese IME such as WeType would swallow so nothing changed. Caps now toggles between English and the input method you last used, and no longer wedges when a key-up packet is dropped (which is what forced you to press it several times).
- macOS: closing the MacBook lid (or unplugging a monitor) now removes that display from the layout instead of leaving a phantom screen. The Mac re-checks its displays when the configuration changes and re-announces, instead of advertising the list it captured at startup. Re-opening the lid (or replugging) now restores the display to the exact spot you had arranged it — it's matched by resolution and remembered across the disconnect, so the controller can reach it again instead of the screen coming back in the wrong place (or not at all).

- macOS: opening MyKVM while it is already running (a second .app copy, `open -n`, or launching from a mounted DMG) now brings the running window to the front instead of starting a second process that fights the first over the network ports.
- Windows: keyboard and mouse from the controller now keep working while a Remote Desktop session owns the machine and after it disconnects, so you can unlock the physical screen remotely instead of walking over to it (#21). The lock-screen input service now follows the physical console session when Remote Desktop swaps it, and the app reaches the service across that swap.

- Keyboard, mouse, and clipboard could fail to connect between machines — the QUIC handshake rejected the peer with `invalid peer certificate: BadSignature`. The transport now pins the device's advertised certificate directly instead of running brittle chain validation over a self-signed certificate, which fixes cross-platform (macOS ↔ Windows) handshakes.

## v0.4.0

### Added

- Update indicator in the title bar: a download icon appears next to "MyKVM" when a newer version is available — click it to open the update panel.

### Fixed

- "Latest version" in Settings now shows the latest released version once a check completes, instead of staying blank when you are already up to date.
- Corrected the clipboard sync description: images are synced too; only file clipboards are unsupported.

## v0.3.4

### Added

- Encrypted QUIC transport for keyboard, mouse, and clipboard traffic (TLS 1.3, pinned to the paired device's certificate).
- In-app updates: check GitHub Releases and install the latest version without leaving MyKVM.
- Clipboard image sync — copy a picture on one machine and paste it on the other (text was already supported).
- Roam across a remote machine's multiple monitors.
- Cross-platform installers for macOS, Windows, and Linux, built automatically on each release.
- Signed macOS builds, so the Accessibility permission survives app updates.

### Improved

- Smoother, more seamless mouse hand-off when crossing between machines and displays.
- Better modifier-key remapping between macOS and Windows.
- Smoother slide-back when MyKVM is not the front window on macOS.
- More reliable LAN discovery and manual peer connection.

### Fixed

- Trackpad two-finger scrolling on the Settings page.
- Faster, more reliable Windows clipboard sync.

## v0.1.0

- Added server/client onboarding and display layout editing.
- Added LAN discovery, manual peer connection, and shared input transport.
- Added text clipboard sync.
- Added English and Simplified Chinese UI strings.
- Added light, dark, and system theme modes.
- Added configurable single-port UDP transport with fallback.
- Added opt-in app performance monitoring.
- Added GitHub Actions CI and tag-based desktop release builds.
