```
   █████████    █████████   ██████   █████ ███████████ ██████████ ███████████  
  ███▒▒▒▒▒███  ███▒▒▒▒▒███ ▒▒██████ ▒▒███ ▒█▒▒▒███▒▒▒█▒▒███▒▒▒▒▒█▒▒███▒▒▒▒▒███ 
 ███     ▒▒▒  ▒███    ▒███  ▒███▒███ ▒███ ▒   ▒███  ▒  ▒███  █ ▒  ▒███    ▒███ 
▒███          ▒███████████  ▒███▒▒███▒███     ▒███     ▒██████    ▒██████████  
▒███          ▒███▒▒▒▒▒███  ▒███ ▒▒██████     ▒███     ▒███▒▒█    ▒███▒▒▒▒▒███ 
▒▒███     ███ ▒███    ▒███  ▒███  ▒▒█████     ▒███     ▒███ ▒   █ ▒███    ▒███ 
 ▒▒█████████  █████   █████ █████  ▒▒█████    █████    ██████████ █████   █████
  ▒▒▒▒▒▒▒▒▒  ▒▒▒▒▒   ▒▒▒▒▒ ▒▒▒▒▒    ▒▒▒▒▒    ▒▒▒▒▒    ▒▒▒▒▒▒▒▒▒▒ ▒▒▒▒▒   ▒▒▒▒▒ 
```                                                                               

A fast, keyboard-driven terminal interface for [Canvas LMS](https://www.instructure.com/canvas), built first for the University of Florida's Canvas (`ufl.instructure.com`).

The Canvas web page for one assignment makes 183 requests (1.4 MB) before the assignment shows up. canter skips the web app and makes only the reads each screen needs:

```
Canvas › GPU Computing                                          Updated 4:54 PM
Due                  Assignment                       Status             Points
Sun Oct 18 11:59 PM  Lab-2 Assignment                 Not submitted      10 pts
Mon Oct 5 11:59 PM   Lab-1 Assignment                 Graded 9/10        10 pts
```

## Install

canter runs on Windows, macOS and Linux. Sign-in needs a Chromium-based browser: Edge, Chrome or Chromium, Brave, Opera, Opera GX, Vivaldi or Arc. Any one of them will do.

**Windows** (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/taherakolawala/canter/releases/latest/download/canter-installer.ps1 | iex"
```

**macOS and Linux:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/taherakolawala/canter/releases/latest/download/canter-installer.sh | sh
```

The installer puts `canter` on your `PATH`. Open a new terminal and type `canter`. To update later, run the same command again.

**With Rust installed:** `cargo install canter`.

You can also download the archive for your system from the [releases page](https://github.com/taherakolawala/canter/releases) and put `canter` anywhere on your `PATH`.

> The Windows build isn't code-signed yet, so SmartScreen may warn about an unknown publisher the first time it runs.

## What it does

It covers sign in (GatorLink + Duo) → course list → assignment list → assignment details (deadline, submission status, description), course files, and announcements.

- **Home screen.** The course list, and under it the widgets you choose: Calendar, To do (missing work and what is due this week), Inbox, Announcements, Grades and Recent files. Each comes small, medium or large, and a bigger one shows more. `E` arranges them.
- Every screen shows a saved copy right away, with **Syncing…** at the top while it fetches fresh data. If the fetch fails, the screen says **Outdated** instead of pretending the old copy is current.
- **Files.** Each course has a Files tab that mirrors its Canvas folders, and each assignment lists its attachments and links. Files download into `canter/<course>/Class Files` or `Assignment Files/<assignment>` in your home folder and open in their default app. A file used in both places is stored once.
- **Announcements.** A per-course tab lists all announcements newest-first, with author, posting date and Canvas read status. Open one to read its message and attachments; replies open in the browser. Opening a readable message marks it Read locally in the TUI, remembered across restarts. Canvas read status is unchanged. Files are fetched only when you choose them and go in `Announcement Files/<announcement-id>`.
- File submissions are available from an assignment that accepts uploads. Text entry, modules, pages and discussions open in the browser for now. Quizzes and assessments are out of scope (see `docs/build-plan.md`).
- To use a separate Canvas site such as Canvas Lite, set `CANTER_ORIGIN` to its HTTPS origin before launching. Its sign-in opens a visible browser and saves a separate, protected session.

## How sign-in works

Personal access tokens aren't used. canter runs the normal UF login itself, in a hidden browser window it drives over the DevTools protocol. You type your GatorLink username and password and approve Duo, all inside the terminal. `canter --login --window` opens a small sign-in window you drive yourself instead. Use it for a password manager, a passkey, a security key or a Duo phone call. Canvas sites other than UF use the window by default.

The browser runs with its own profile, separate from your everyday one, and closes once you're signed in. Set `CANTER_BROWSER` to a browser's full path if canter doesn't find yours.

The session is saved so later runs open without a browser until Canvas or UF expire it:

| System | Where | Protected by |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\canter\ufl.session.dpapi` | DPAPI, for your Windows user |
| macOS | `~/.local/share/canter/ufl.session` | Encrypted with a key kept in your Keychain |
| Linux | `~/.local/share/canter/ufl.session` (or `$XDG_DATA_HOME/canter`) | Encrypted with a key kept in the Secret Service (GNOME Keyring, KWallet) |

Where no keychain is running (WSL, servers, minimal desktops), the key goes in `session.key` next to the session instead, readable only by you, and canter says so once when it exits.

Passwords are never saved or logged.

The program was called TUICanvas until 2026-10-27. If a folder from that name is still there, the first run moves it across (the saved session, the saved copies and the file library), so nothing has to be signed in or downloaded again. A folder already under the new name is never overwritten.

## Usage

```
canter                 Open the reader (signs in first if needed)
canter --login         Sign in again (--window to drive the sign-in window yourself)
canter --logout        Delete the saved session and this account's saved copies
canter --diag          Log request timings (no content) to the data folder
canter --version       Print the version
canter --help          Everything else, including the developer options
```

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, PgUp/PgDn, `g` `G` | Move or scroll |
| `Enter` / `→` | Open (a file opens in its default app, downloading it first if needed) |
| `E` | On the home screen, arrange the widgets under the course list. The arrows select one, `Enter` picks it up so the arrows move it (`←`/`→` to the other side), `+`/`-` change its size, `a` adds a widget, `x` takes one off and `Esc` finishes. The arrangement is saved for your account. (`Space` then `h` hides a course.) |
| `s` | On an assignment, start a file submission in your home folder. Type a name or paste a full path; matching local files and folders appear as you type. Use `↑`/`↓` and `Tab` to complete, then `Enter` to add each file. `Ctrl+U` clears the path. After adding files, press `Enter` at the starting folder to review, `y` to submit, or `Esc` to cancel. Files are checked against Canvas's allowed extensions and upload size rules. |
| `h` | On an assignment, open past submissions. Use `←`/`→` to switch attempts and `↑`/`↓` to scroll the selected attempt. `r` refreshes; `Esc` returns to the assignment. |
| `Esc` / `←` / Backspace | Back (or clear the filter or selection) |
| `Tab` / `Shift+Tab` | On the home screen: move between the course list and each widget. In a course: cycle Assignments, Files and Announcements forward/backward. In an assignment or announcement: select the next/previous file or link (`1`–`9` jump to one) |
| `d` / `D` | Download the selected file (or folder) / everything here |
| `e` | Show the file or its folder in File Explorer, Finder or your file manager (on the course list: your canter folder) |
| `/` | Filter the current list (in Files: search the whole course) |
| `r` | Refresh now |
| `o` | Open the current item, or the selected link, in your browser |
| `L` | Sign in again |
| `q` | Quit |

## Layout

| Path | Contents |
| --- | --- |
| `src/` | Rust app: Ratatui UI, Canvas client, session and snapshot store |
| `tools/` | Python sign-in helper, login probe, HAR analyzer (with tests) |
| `docs/` | Build plan, discovery notes, authentication notes, request policy |
| `benchmarks/` | Baseline capture method and findings (raw captures stay in the ignored `private/` folder) |
| `scripts/` | Development check and watcher scripts |

## Building from source

Requirements: Rust 1.89 or newer. On Windows the GNU toolchain is fine and Visual Studio isn't needed. On Linux you also need `pkg-config` and the OpenSSL headers (`libssl-dev` or `openssl-devel`).

```sh
git clone https://github.com/taherakolawala/canter
cd canter
cargo install --path .     # builds a release copy and puts canter on your PATH
```

`cargo test` runs the offline tests. The developer options (`--smoke`, `--dump`, `--bench`) write under `benchmarks/private` in the checkout. The old Python sign-in helper (`canter --login --python`, Windows only) needs `py -m venv .venv` and `.venv\Scripts\python.exe -m pip install -r tools\requirements-auth.txt` first.

Current progress, known issues and next steps: [docs/status.md](docs/status.md). How it works and which optimizations are in place: [docs/architecture.md](docs/architecture.md). Measured against the Canvas website: [benchmarks/baseline-2026-10-20.md](benchmarks/baseline-2026-10-20.md).

## Privacy

- Apart from sign-in, which goes through your school's own login page in the browser, canter talks only to your Canvas site and the hosts Canvas stores files on. There is no telemetry.
- Diagnostics (`--diag`) record only endpoint templates, statuses, sizes and timings. They never record URLs with IDs, cookies or response bodies.
- Saved copies of course data live in the data folder above, under `cache`, separated by Canvas origin and account.
- Downloaded files live in `canter` in your home folder (set `CANTER_LIBRARY` to move it). Canvas cookies are sent only to Canvas, never to its file storage hosts.
- Raw HAR captures, session files and private reports are ignored by git.

## Disclaimer

canter is an independent project. It is not affiliated with, endorsed by or supported by Instructure (the maker of Canvas) or the University of Florida. It uses your own Canvas session and only does what you could do on the Canvas website yourself.

## License

[MIT](LICENSE)
