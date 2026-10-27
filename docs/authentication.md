# Authentication experiment

User decision: terminal-first username/password and Duo. Browser login is an accepted fallback. Later launches should reuse valid protected session state without prompting where UF/Canvas policy permits.

Verified without credentials: a standard HTTPS client reaches UF's Shibboleth session-storage initialization page and then the normal `j_username` / `j_password` form. The native probe correctly declares no browser localStorage support. No MFA or access-control checks are skipped.

The Duo stage cannot be inspected meaningfully without a user login. UF's public documentation describes Duo Universal Prompt and Verified Push. We cannot simply use Duo's integration API: those APIs require integration credentials owned by the institution/application, which this client does not have.

## Current probe

`tools/auth_probe.py` is temporary development tooling, not the final Rust app. It uses Python's standard library. Default mode reaches the public credential form without submitting a username/password:

```powershell
python tools/auth_probe.py
```

Run the next experiment yourself in an interactive terminal:

```powershell
python tools/auth_probe.py --login
```

The username/password are entered locally; password input is hidden. The probe submits once to the exact UF HTTPS origin, then stops to identify the next stage. This may initiate the usual Duo flow, but the probe does not yet complete Duo or persist the session. It does not retry rejected credentials.

To see password characters while typing, use `python tools/auth_probe.py --login --show-password`. This explicit option displays the password in your terminal; it does not add the password to the diagnostic report. Hidden input remains the default.

The output `benchmarks/private/auth-stage.json` contains only the stage, host, and form field names/types. No field values, cookies, URL parameters, raw HTML, or passwords are saved. Keep credentials out of chat. The probe refuses non-interactive credential input, unknown redirect origins, and preserved-body redirects that could forward a credential POST.

Next: use the stage report to determine whether terminal Duo interaction can be implemented or whether the accepted browser fallback is needed. Implement persistent OS-protected session reuse and native Canvas reads after the login path is established. Do not call this a complete login system yet.

## Probe v2 correction

The first credentialed run returned to the username/password form. Inspection identified a concrete client bug: UF's `_eventId_proceed` control is a `<button type="submit">`, but the first parser recorded only `<input>` elements. The login POST therefore omitted the clicked submit action. This is not evidence of incorrect credentials or unsupported terminal authentication.

The regression test reproduced the missing POST field. Version 2 records button controls, includes only the selected login submitter, and refuses to send credentials if that action is absent or disabled. Reports include a version and timestamp so the next credentialed result can be distinguished from the old report. Live public-form verification is performed without transmitting credentials; a user-run retry is necessary to inspect the post-login Duo stage.

## References

- UF authentication changes: https://it.ufl.edu/get-connected/multi-factor-authentication/upcoming-changes/
- UF remembered-device behavior: https://it.ufl.edu/get-connected/multi-factor-authentication/remember-me-feature/
- Duo integration requirements: https://duo.com/docs/duoweb

## Terminal browser-assisted login experiment

The credentialed probe v2 reached `duo_challenge` on Duo's origin with no HTML forms. The user then completed the terminal helper's Verified Push and remembered-device choice. Canvas login succeeded, and a native profile read returned HTTP 200. A subsequent saved-session check also succeeded without launching a browser. Session lifetime beyond this immediate reuse remains unmeasured.

The helper renders the normal login using an isolated headless Edge engine. Known Duo stages have a concise terminal presentation: loading updates automatically, Verified Push shows the code and instructions, remembered-device choice uses `y`/`n`, and success continues automatically. While waiting for Verified Push, `o` opens other methods, `m` exposes the full prompt controls, and `q` cancels. Unknown challenges preserve their instructions and numbered controls. Prompt text and verification codes are not written to diagnostic reports. Use the visible-browser fallback if a prompt requires browser capabilities.

```powershell
.\.venv\Scripts\python.exe -m tools.terminal_login --show-password
```

Hidden password input is the default if `--show-password` is omitted. Playwright 1.64.0 is installed in the project's isolated environment; the helper uses the existing Edge installation. `--channel chrome` selects the installed Chrome instead. `--browser` uses the accepted visible-browser login fallback.

After Canvas login, the helper tests a native HTTPS profile read. Successful authentication state is encrypted with Windows DPAPI for the current user and stored at `%LOCALAPPDATA%\canter\ufl.session.dpapi`. If browser login succeeds but native reads do not, the protected browser state is still retained and the diagnostic report records the distinction. No assertion of native authentication is made from the page URL alone.

Check session reuse without launching a browser:

```powershell
.\.venv\Scripts\python.exe -m tools.terminal_login --check
```

The helper uses stored sessions first on normal runs as well. `--fresh` ignores saved state for a new login. Reuse still depends on UF/Canvas expiry and MFA policy; exported cookie/localStorage state does not promise preservation of every browser remembered-device signal.

The new diagnostic report is `benchmarks/private/auth-login-result.json`; it contains stage/status metadata only. The old probe remains useful for isolated form inspection. The Rust reader reads this same protected session and runs the helper whenever sign-in is needed (see [status.md](status.md)).

Automated checks cover cookie scope, expiration/path matching, rotated/deleted cookie handling, native origin restrictions, CSRF scope, terminal-control filtering, response redaction, Windows encryption round trips, and replay of the observed Duo screen sequence. The streamlined presentation has been tested against synthetic versions of the observed screens; a new cold login has not been triggered solely to retest its appearance. The original helper's terminal Duo completion and native reads were live-verified by the user's run, followed by our successful native saved-session check.

## Sign-in in Rust

The reader signs in itself, with no Python and no Playwright. It drives Microsoft Edge over the DevTools protocol: a real browser rendering UF's own login, controlled from the terminal. The Python helper stays in the tree and still works (`canter --login --python`).

Two modes share the code in `src/signin.rs`.

**Background (the default for UF).** Edge runs with `--headless=new`, so there is no window. canter reads each page, asks for the GatorLink username and password in the terminal, fills UF's form, clicks UF's submit button, and shows every Duo screen on one status line, the same presentation the Python helper has: loading passes by on its own, Verified Push shows the code, the remembered-device question asks `y`/`n`, `o` opens other options, `m` shows the full prompt, `q` cancels. An unfamiliar challenge keeps its own instructions and numbered controls rather than being given a meaning it may not have.

**Window (the fallback, and the default for any other Canvas site).** `canter --login --window` opens a separate, phone-sized sign-in window (Edge with `--app=<url>` and `--window-size=420,760`), not a tab in a browser that is already open. The user drives it, so a password manager, a passkey, a security key and a Duo phone call all work. canter reads nothing from that page and types nothing into it; it watches for the Canvas session and closes the window as soon as sign-in finishes. The window is reachable on purpose, not only after a failure: by the flag, or by `CANTER_SIGNIN=window`.

`CANTER_SIGNIN` selects the default path: `background`, `window` or `python`. `CANTER_BROWSER` names an exact browser executable, and `--chrome` uses an installed Chrome instead of Edge.

### What it does with the password

The username and password live in memory only long enough to reach the page. The password is typed with the protocol's `Input.insertText` after the field is focused, so it never appears in a script, a log line, a diagnostic report or a file. Password input is hidden unless `--show-password` is given; a terminal that cannot hide it is refused rather than echoing it. Nothing is typed into a page whose URL changed since it was read, and nothing is ever typed into a Canvas page.

### Where the session goes

The browser's cookies are read with `Storage.getCookies`, converted to the session file's existing cookie shape (the same field names and leading-dot domain convention Playwright's storage state uses), and confirmed with one `/api/v1/users/self/profile` read, not by looking for something on Canvas's page. The result is written through `src/protect.rs` to the same DPAPI-protected file at `%LOCALAPPDATA%\canter\<host>.session.dpapi`, with any fields an earlier Python-written file had (its `origins` list, for example) carried over untouched. Nothing downstream changes.

The sign-in browser uses its own profile folder, `%LOCALAPPDATA%\canter\signin\<host>`, so the user's real browser profile, extensions and sessions are untouched. Keeping that folder between runs is what lets UF's and Duo's remembered-device cookies survive; a saved session's cookies are also restored into the browser before it starts, for the same reason. One consequence of a separate profile: a password-manager extension installed in the user's own browser is not present in the window mode, though the operating system's autofill and passkeys still are. Browser local storage is not captured by the Rust path; the reader needs only cookies, and a file the Python helper wrote keeps its `origins` list.

### Failures, told apart

Each of these is reported differently, because what to do next differs: a password UF rejected (its form comes back after a submit, and nothing is retried automatically), Duo denied, a Duo request that ran out of time, no Edge or Chrome installed, the window closed or the browser gone, a network failure, a page outside the UF/Duo/Canvas origins, Canvas accepting the browser but not a direct API read, and the overall ten-minute limit.

### What breaks if UF changes its page

The background mode knows four things about UF's and Duo's pages. If UF changes any of them, the window mode still works and is the fallback:

- the field names `j_username` and `j_password`, and the submit control `[name="_eventId_proceed"]` (a `<button>`, which is why it is clicked rather than the form being submitted);
- the start URL `https://ufl.instructure.com/login/saml/355`;
- the wording Duo's screens use, which decides which short status line is shown: "Enter code in Duo Mobile", "Check for a Duo Push", "Is this your device?", "Success!", and the denied and timed-out wordings. Unrecognised wording falls through to the interactive prompt, so a changed screen is shown rather than mishandled;
- that sign-in stays on `ufl.instructure.com`, `login.ufl.edu` and `*.duosecurity.com`. Any other origin stops the background mode.

The page is read in the main frame and in frames of it that are same-origin; a cross-origin frame cannot be read, which is why Duo's own page (a top-level navigation today) is what the status lines describe.

The protocol side depends on Edge writing its chosen port and endpoint path to `DevToolsActivePort` in the profile folder, and on `Target.getTargets`, `Target.attachToTarget`, `Runtime.evaluate`, `Input.insertText`, `Storage.getCookies` and `Storage.setCookies`. Those methods have been stable for years, but they are Chromium's, not a standard.

One limit: the browser is closed when sign-in ends, either way, but a reader that is killed outright cannot close it. A sign-in window left behind that way can be closed by hand; the next sign-in reuses the same profile folder.

### Verified live without credentials

Run on 2026-10-22 on the owner's machine, stopping at the password prompt so nothing was sent: Edge was found and started with the dedicated profile, wrote `DevToolsActivePort`, accepted the WebSocket handshake, and answered `Target.getTargets` and `Target.attachToTarget`. The page was navigated to UF's SAML start and read, the GatorLink form was recognised, and the terminal showed `Connecting to Canvas…` then `Sign in with your GatorLink account`. `--login --window` opened its own Edge window and showed `Finish signing in in the sign-in window`. Nothing beyond the public form was exercised.

### Live verification still to do

Everything after the form is unverified: filling the password, UF's submit, every Duo screen, reading the cookies, the native check, saving the session, closing the window on success, and each failure case. The owner's credentials were not available, and the tests never touch the network. These are the runs to make, in this order.

```powershell
cargo test
target\release\canter.exe --logout
target\release\canter.exe --login
```

What to look for on `--login`: one status line that updates in place (`Connecting to Canvas…`, then the GatorLink prompt, then `Checking your password…`), a Duo line that names the push or shows the three-digit code, the remembered-device question answered only by `y`/`n`, and a final `Signed in. Your session is saved for next time.` No window should appear, no `msedge.exe` should be left running afterwards, and `%LOCALAPPDATA%\canter\ufl.session.dpapi` should have a new timestamp. Then `target\release\canter.exe --smoke` should pass with that session.

Then the failure paths, one at a time: a deliberately wrong password (expect "UF did not accept that sign-in", and a prompt that does not retry on its own), a denied Duo request, and a Duo request left to expire. Then the window mode:

```powershell
target\release\canter.exe --logout
target\release\canter.exe --login --window
```

A single small window (about 420 by 760) should open on its own, outside any browser already running, and close itself a second or two after Canvas loads. Closing it by hand should report "The sign-in window was closed." Finally, `$env:CANTER_SIGNIN = "python"` with `canter --login` should still run the Python helper unchanged.
