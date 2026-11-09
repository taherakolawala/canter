<p align="center">
  <img src="assets/canter.svg" alt="canter" width="804">
</p>

canter is a Rust-based terminal (TUI) client for Canvas by Instructure. It shows your courses roughly 6 to 10 times faster than the Canvas website.

<p align="center">
  <a href="#setup">Setup</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#support">Support</a>
</p>

## Setup

**Windows** (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/taherakolawala/canter/releases/latest/download/canter-installer.ps1 | iex"
```

**macOS / Linux:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/taherakolawala/canter/releases/latest/download/canter-installer.sh | sh
```

Open a new terminal and type `canter`. On first run, paste your school's Canvas address or login link (UF: just press Enter). Signing in needs Edge, Chrome, Brave, Opera, Vivaldi or Arc.

## How it works

- **Only what's needed.** The Canvas website makes about 180 requests to show one assignment. canter skips the website and asks Canvas's API for just the data on screen.
- **Instant screens.** Everything you've opened is saved locally, so screens appear immediately while fresh data loads behind them.
- **Smaller reads.** Lists are fetched with queries that ask only for the fields shown, often a tenth of the size.
- **In parallel.** Several reads run at once, downloads run separately, and files you already have open from disk.
- **Signed in once.** You sign in through your school's normal login in a browser window. The session is saved securely (DPAPI on Windows, the system keychain on macOS and Linux) and reused.

## Support

- **University of Florida:** full support.
- **Other schools:** preliminary support. Enter your school's Canvas address or login link on first run, or with `canter --school`.
- **Features:** all of Canvas except quizzes and assessments, which are left out for academic honesty.

---

<sub>Not affiliated with Instructure or the University of Florida · [MIT License](LICENSE)</sub>
