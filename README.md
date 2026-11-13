# Mouse without borders

A small, free utility that lets **one mouse and keyboard control up to three PCs** on the
same Wi-Fi or Ethernet network. Move the cursor off the edge of one screen and it appears on
the next computer. Copied text and copied files follow you too.

Developed by **Prof. Dattaraj S. Vidyasagar** and **Prof. Yash D. Vidyasagar**
for **Vidyasagar Academy** · <https://vsa.edu.in/software/> · softinquiry@vsa.edu.in

Licensed under the **GNU General Public License v3.0** (see `LICENSE`).

## How it works

- One PC is the **master**: its own mouse and keyboard control the others.
- The other PCs (**secondary**) find the master automatically on the network.
- Every PC uses the same fixed security key (`123456`). A secondary uses it only the first
  time it connects; after that the pairing is remembered.
- Everything sent between the PCs is encrypted.
- **Clipboard:** copied text is shared across all PCs.
- **Files:** copy a file on one PC and it is saved in `Desktop\Shared Files` on the other
  PCs, ready to paste.
- **Layout:** on the master, drag the computer pictures in the *Computer Matrix* to match
  how the screens sit on your desk (side by side, above and below, any mix). Tick *Two Row*
  for a second row.
- Closing the window keeps the program running in the system tray.
- The program **starts by itself when you sign in to Windows** and waits quietly in the
  tray (switch this off on the *Other Options* tab if you prefer).

Hotkeys on the master: `Ctrl+Alt+F1` controls this PC, `F2` and `F3` the other PCs.

## Using it

1. Install the program on every PC (or copy `MouseWithoutBorders.exe`).
2. On the PC whose mouse and keyboard you want to use, choose **Master**, then **Apply**.
3. On the others, choose **Secondary**, then **Apply** (the app restarts).
4. If Windows Firewall asks, choose **Allow**. If it never asks, or the PCs do not find
   each other, press **Allow through firewall** on the *IP Mapping* tab (most important
   on the master) and answer **Yes**.
5. On the master, drag the computers into place in the Computer Matrix and press **Apply**.

Every PC must run the **same version**. After that nothing more is needed: restart the
PCs in any order and they reconnect by themselves within a few seconds of signing in.

### If the PCs do not connect

The line at the bottom of the window says what is wrong. The usual causes:

- **Two masters.** A fresh installation starts as Master. Only one PC may be the master;
  set the others to Secondary.
- **Firewall.** Use the *Allow through firewall* button, first on the master.
- **Different versions.** Install the same version everywhere.
- **Different networks.** Guest Wi-Fi and some routers ("AP isolation") keep devices
  apart. Put the PCs on the same normal network. As a last resort type the master's
  address (shown on its *IP Mapping* tab) into *Master IP* on each secondary.

### Limits

- The program runs after you **sign in**; it cannot type on the Windows sign-in or lock
  screen of another PC, and it cannot click in administrator prompts or in programs that
  were started "as administrator".
- Only the main screen of each PC is used.
- A PC with no mouse plugged in may hide its cursor (a Windows habit); plug in any mouse
  or switch on *Mouse Keys* in Windows settings.

## Building (Windows 10 / 11)

Needs [Rust](https://rustup.rs) (MSVC toolchain) and [pnpm](https://pnpm.io).
Double-click `Build_Release.bat`. The installer and the portable EXE appear in the
`Release` folder.

Manual steps: `pnpm install`, then `pnpm tauri build`. Core tests: `cargo test -p mwb-core`.

## Project layout

- `core/` – Rust engine: protocol, pairing, encryption, discovery, layout, clipboard and files.
- `src-tauri/` – Tauri v2 shell (window, tray).
- `src/` – settings window in plain TypeScript.

## Version history

| Version | Notes |
|---|---|
| 0.2.0 | Reliability: starts with Windows (hidden in the tray), only one copy runs, PCs find each other through the firewall and on PCs with several network adapters, reconnect after restart or sleep, self-repairing pairing, no stuck keys or buttons, no switching in the middle of a drag, firewall button, clear messages when something is wrong |
| 0.1.3 | Installer and uninstaller now use the application icon |
| 0.1.2 | New application icon |
| 0.1.1 | Fix: cursor on a secondary PC could stop at an invisible wall (pointer speed differences) |
| 0.1.0 | Settings window, tray icon, draggable layout, installer |
| 0.0.1 | Core engine and command-line test program |
