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

Hotkeys on the master: `Ctrl+Alt+F1` controls this PC, `F2` and `F3` the other PCs.

## Using it

1. Install the program on every PC (or copy `MouseWithoutBorders.exe`).
2. On the PC whose mouse and keyboard you want to use, choose **Master**, then **Apply**.
3. On the others, choose **Secondary**, then **Apply** (the app restarts).
4. Allow Windows Firewall on **private networks** when asked.
5. On the master, drag the computers into place in the Computer Matrix and press **Apply**.

If automatic search does not work on your network, enter the master's address on the
*IP Mapping* tab of each secondary.

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
| 0.1.2 | New application icon |
| 0.1.1 | Fix: cursor on a secondary PC could stop at an invisible wall (pointer speed differences) |
| 0.1.0 | Settings window, tray icon, draggable layout, installer |
| 0.0.1 | Core engine and command-line test program |
