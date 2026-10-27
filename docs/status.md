# Project status

Last updated: 2026-10-22. Update this file whenever a milestone moves.

## Where things stand

How the pieces fit together is described in [architecture.md](architecture.md). Milestones 1–3 of [the build plan](build-plan.md) are done, and so are Milestones 7 and 8 of version 2 (the file library, Class Files and assignment attachments). Milestone 4 is partly done through the saved copies that came with Milestone 2; Milestones 5 and 6 have not started.

| Milestone | State | What exists |
| --- | --- | --- |
| 1. Sign-in and request experiment | **Done**; Rust sign-in awaiting a live run | Terminal sign-in (GatorLink + Duo), now in Rust driving Edge over the DevTools protocol, with a separate phone-sized sign-in window as the fallback and the Python helper kept as a last resort; live-verified native REST and GraphQL reads; failure classification; [request policy](request-policy.md) |
| 2. Reader vertical slice | **Done** | Courses → assignments → assignment detail, with saved copy + Syncing, refresh, back, open in browser |
| 3. Rendering and network baseline | **Done** | Network, parse and draw timed separately (`--diag`, `--bench`); automated website baseline; the reader shows fresh data 6–10× sooner than the website ([baseline](../benchmarks/baseline-2026-10-20.md)) |
| 4. Selective resource reuse | Partly | Saved copies for courses, assignments, files and announcements (always re-read on entry) |
| 5. Prediction experiment | Not started | |
| 6. Hardening and delivery | Not started | |
| 7. Local file library | **Done** | `%USERPROFILE%\canter\<course>\Class Files` and `Assignment Files`; one stored copy per file shown in several places through hard links; change detection; safe names; 30-day clean-up of automatic images; downloads restricted to Canvas and its storage hosts. Verified live: download, install, hard link, internet mark. |
| 8. Class Files and assignment attachments | **Done** | Files tab per course (folders, search across the course, download one, a folder or all, show in Explorer); files & links list on each assignment, numbered like the text; images downloaded with the assignment; other assignments open in the reader, everything else in the browser |
| 9. Sign-in for other Canvas sites | Implemented, live check pending | The login helper uses an origin-scoped visible browser outside UF, with a separate protected session. Optional for the first live upload. |
| 10. Submissions (file upload, text entry) and instructor feedback | In progress | File-path upload UI and streamed Canvas API flow; text-entry and website-URL submissions through the same review-and-confirm screen; instructor comments, comment attachments and rubric scores on the assignment page. Offline tests pass. A live submission to a user-selected UF assignment remains, and `online_url` cannot be checked live until an assignment uses it. |

## What works today

- **Sign-in.** canter itself runs UF's normal login in a hidden Edge window, driven from the terminal over the DevTools protocol (`src/signin.rs`); no Python and no Playwright. Offline tests pass; **no live run yet** (see [authentication.md](authentication.md) for the checks to make).
  - You type your GatorLink password and approve Duo (plain push or Verified Push), with progress shown on one spinner line.
  - `canter --login --window` instead opens a separate phone-sized sign-in window you drive, which is what a password manager, a passkey, a security key or a Duo phone call needs; it closes itself when sign-in finishes. That is also the default for Canvas sites other than UF (`CANTER_SIGNIN=background|window|python`).
  - `tools/terminal_login.py` is still there as a last resort: `canter --login --python`.
  - The session is encrypted with Windows DPAPI under `%LOCALAPPDATA%\canter`.
  - The app starts sign-in itself if the saved session has expired at launch. Mid-session expiry shows a banner and waits for `L`.
- **Reader** (`target\release\canter.exe`).
  - Shows course list, assignment list (due date, status, points) and assignment detail (deadline, availability, status, attempts, description with links).
  - **Keyboard.** A fixed set of keys means the same on every screen: `↑↓` move, `Enter` open, `Esc` back, `Tab` next tab or section, `/` search, `r` refresh, `o` browser, `Space` actions, `?` help, `q` quit. `Space` opens a menu of everything that applies to the selection and the screen, each with the letter that runs it, and `/` searches it; `?` lists every key on the current screen. All keys, hints, the menu and the help come from one table (`src/keys.rs`), and tests fail if a key does two things in one place or a core key changes meaning.
  - **Calendar.** The home screen lists the courses across the top and a month calendar below, like Canvas's: each day shows what is due or scheduled (assignments, quizzes, discussions, calendar events, planner notes) in the course's Canvas colour, finished items dimmed and missing ones marked `!`. `Tab` gives it a day cursor: arrows move by day and week, `PgUp`/`PgDn` change month, `Home` returns to today, and the selected day's entries are listed under the grid; `Enter` opens an assignment in the reader (choosing first if the day has several) and anything else in the browser. Each month shown is one planner read (`/api/v1/planner/items`, about 0.8 s), plus one read of the course colours, with saved copies shown meanwhile. Hidden courses are left out.
  - **Hiding courses.** On the course list, `Space` then `h` hides the selected course; `Space` then `H` lists hidden courses (marked) so one can be restored the same way; `Esc` stops listing them. Hidden course choices are local to the account and survive refreshes and restarts.
  - **Course tabs follow Canvas.** A course shows Home, Assignments, Announcements, Files, Modules and Grades only when Canvas shows them to students, in Canvas's order, and opens on the first one. Home is the course's Canvas home: when that is a page (13 of 17 courses here; all 13 home pages read live in 0.25–0.47 s, and every course now shows at least one tab), the **Home** tab shows it, with its links opening in the reader; on Home, `Tab` switches course tabs and `1`–`9` reach its files & links. A home set to Modules or Assignments shows that section first, as Canvas does, even with its own tab hidden; a syllabus or activity-feed home is not shown yet. `Tab` / `Shift+Tab` cycle what is shown; a course that shows none of them says so and `o` opens it in the browser. The tab list (`/api/v1/courses/:id/tabs`, about 0.4 s) is saved and used at once, and read from Canvas once per run of canter and on `r`, not on every visit.
  - **Files.** Files mirrors the course's Canvas folders; `Enter` opens a file in its default app (downloading it first if needed), `d` downloads, `D` downloads everything in the folder, `e` shows it in File Explorer, `/` searches every folder. Each file shows whether it is saved, newer on Canvas, downloading or locked.
  - **Announcements.** Per-course list and details, newest posting date first; author/date/Canvas read status, saved copy + Syncing/Outdated, keyboard links and attachments. Full metadata pagination avoids dropping older-created announcements published recently. Body HTML is fetched only on opening. Files and inline images download only when chosen into `Announcement Files/<id>`. Replies open in the browser; reading marks it Read in the TUI across restarts without mutating Canvas read status.
  - **Search.** `/` or `Ctrl+K`, from any screen, opens one box that finds anything the reader knows about: a course, an assignment, an announcement, a page, a file, a conversation, or an action to run. The query is matched as a subsequence, so `cop46 l2` reaches "COP4600 - Lab 2", and the characters that matched are underlined so it is clear why a result is there. `Enter` opens what is selected on top of the screen you were on, so `Esc` comes back; `Ctrl+U` clears the box. This replaced narrowing a list in place with `/`, which only ever reached the screen you were already looking at.

    **Narrowing to one class.** `@` and a few letters of a course name or code scopes everything after it to that class: `@cop46 lab 2` means "lab 2, in COP4600". While the tag is being typed the box offers the classes it could mean, best first, so an ambiguous tag shows its alternatives instead of guessing silently; once it resolves, the line under the box names the class it narrowed to. A tag that names no class finds nothing rather than quietly searching everything.

    **Hidden courses** are out of the search by default, along with everything in them, because hiding a course is usually a way of saying it is done. `Ctrl+H` in the box brings them in and is remembered across restarts, and the line under the box says `hidden included` while they are. It is a separate switch from `H` on the course list: one tidies what you look at, the other decides what you can find.

    Searching reads nothing. It can only find what has been read, so every saved copy is loaded from disk the moment the course list arrives — all of them at once, since that is a walk over small files and needs no waiting — and the box is useful immediately. Each list is then re-read from Canvas in the background, two at a time out of the four the executor allows, so what you search is current rather than whatever was on disk from last week. The box always says how much has arrived (`searching 11 of 17 courses, still reading`), because a search that quietly cannot see half the account is worse than one that admits it. This is the only place the reader asks Canvas for something nobody asked for, and the headless walk-through, the benchmark and the tests leave it switched off so they state exactly what they fetch.

  - **Inbox.** A small box on the home screen — an envelope, the name and how many conversations are unread — kept small on purpose so the room beside the calendar is free for the widgets that come next. `Tab` cycles Courses, the calendar and the box, and `Enter` on it opens the Inbox as a screen of its own: `←`/`→` move between the folders Canvas offers (Inbox, Unread, Starred, Sent, Archived) and `Enter` opens a conversation in the reader, with each message under its author and date, newest first, then files & links. Unread conversations are marked `•` and starred ones `*`; the list is ordered by latest activity. The box's count is Canvas's unread set less the conversations read here, so it falls as you read and may say less than Canvas does. Reading a conversation marks it Read in the TUI across restarts and tells Canvas nothing: the detail read sends `auto_mark_as_read=false`, which is the only thing standing between reading a message here and changing its state on Canvas. Attachments and inline images download only when chosen, into `Inbox Files/<conversation id>`. Saved copies keep the 100 newest conversations and 100 messages in total. `R` replies to everyone in a conversation and `N` on the course list writes to a course's teachers; both open a review that shows the recipients, the subject and the body exactly as they will be sent, and nothing is sent until you press `y`.
  - **Modules.** One list with each module's name as a heading (its state: completed, in progress, locked until a date; "in order" when Canvas requires it), then its items with their type, indent, due date, points and requirement (Done, To do, Locked); text headers show as headings and are skipped by the cursor. `/` searches item and module names. `Enter` opens assignments and pages in the reader, puts files in the library and opens them, and opens quizzes, discussions, outside links and tools in the browser through their module item. `d` downloads the selected file, `D` every file in the list, `e` shows it in File Explorer. Module files are course files, so they go to `Class Files` under their Canvas folder; where the instructor hides Files the folder cannot be read (Canvas answers 403), so they go to `Module Files/<module>`. One read per visit (`/api/v1/courses/:id/modules` with items, 0.2–2.7 s per course).
  - **Grades.** The Grades tab, shown in 12 of the 17 courses here, lists the course total and letter grade at the top (or says the instructor does not show it), then every assignment under its Canvas group with the group's weight: score out of points, and late, missing, excused, graded or submitted. Rows whose instructor comments have not been opened in canter are marked `*`; opening the assignment clears the mark, and Canvas is never told what has been read. `/` searches assignment and group names, `Enter` opens the assignment and `o` opens it on Canvas. On the course list, `Space` then `g` shows every course's current grade (one switch for all of them, saved locally), which reads the grades of the courses on screen. Three reads per course: the assignment groups with submissions, the course total, and the instructor comments.
  - **Pages.** The Pages tab, shown in 3 of the 17 courses here, lists every page of the course with when it was last changed, the course home marked, newest change first; `/` searches titles and `Enter` opens the page in canter. Course pages are also read from modules or from links in any page, assignment or announcement: title, last update, text, then files & links. Their files go to `Page Files/<title>`. Opening a page here does not mark a module's "view" requirement done; opening the item in the browser does.
  - **Assignment, announcement and course pages.** One scrolling page: details, text, then **Files & links** (numbered like the `[n]` marks in the text) and, on assignments, **Submissions**. `Tab` moves between those sections and `Esc` returns to the text. In files & links, `↑↓` or `1`–`9` select and `Enter` opens (files in their app, other assignments in the reader, anything else in the browser); `d`/`D` download, `e` shows the folder, `o` opens the selected item's page in the browser.
  - **Past submissions** are part of the assignment page. When Canvas shows something was submitted, the attempt history is read with the page (one more read, fresh on each visit). `↑↓` in the Submissions section choose an attempt, shown in full: date, type, status, grade when visible, attached file names and submitted text or URL.
  - **Instructor feedback** is the next section, for the attempt selected. The same submission read brings the comments and the rubric scores, so it stays one request: each comment shows its author, date and text, and `↑↓` move between the comments and then the scored rubric criteria (criterion, points of the possible points, the instructor's note) under the rubric's title and total. Canvas keeps one rubric assessment per submission, so it appears with the latest attempt. Comment attachments go to `Assignment Files\<assignment>\Feedback` with `Enter`, `d` or `D`, through the same one stored copy and hard links as every other file. What Canvas carries no text for is said plainly rather than guessed at: an audio or video comment, and feedback drawn on the submitted file in Canvas's document viewer, are marked as browser-only, and `o` opens the submission page where Canvas shows them.
  - **Submitting.** `s` (or `Space` then `s`) submits files, `t` typed text, and `u` a website address, where Canvas accepts that type. All three end on the same review screen, which shows the exact content that will be sent — the files with their full paths, or the text and address character for character — and only `y` sends it. Text can be typed in canter or read from a UTF-8 file named with `Ctrl+O` (the way to submit paragraphs). Each one is verified by re-reading the submission afterwards.
  - **The name.** The program was TUICanvas until 2026-10-27 and is now canter: the crate, the binary (`canter.exe`), the `CANTER_*` settings and the folders it keeps. The data folder (`%LOCALAPPDATA%`), the file library (`<home>`) and the library's own index folder are each carried over from the old name on first run, by one rename, and only when there is nothing at the new name to overwrite — so the rename costs no sign-in and no re-download.
  - Files go to `C:\Users\<you>\canter` (`e` on a course opens its folder; `Space` then `f` opens the canter folder). Images in an assignment download when you open it and are removed after a week unused.
  - Each screen shows its saved copy at once, with **Syncing** at the top, and is labelled **Outdated** if the fresh read fails.
  - Colours mark deadline urgency and status.
- **Reads**, measured live on one account, 17 courses, 157 assignments:
  - Course list: REST, about 300 ms and 22 KB.
  - Assignment list: GraphQL, median about 300 ms per course. The REST equivalent is about 600 ms with a 9.6 s worst case, and its 944 KB total drops to 64–75 KB. The data matched REST on every assignment.
  - Assignment detail: REST, about 210–250 ms and 4 KB.
  - Grades: three REST reads, 687–768 ms together on one course (`--dump`, 2026-10-22). The assignment groups with submissions are the slowest: median 461 ms across the 17 courses, 2.6 s worst case. The comment read answered 200 in all 17 courses, median 253 ms, and found 12 instructor comments in 4 of them.
  - Page list: REST, about 230 ms.
  - Home screen, all of it (courses, the month's planner, course colours and
    the Unread folder for the box's count): about 900 ms, of which the one
    conversation read is about 560 ms. The individual reads: courses 273 ms,
    planner 794 ms, colours 160 ms, conversations 560 ms. That read was
    measured against the Inbox folder, which the home screen read before the
    box replaced the old section; Unread is the same endpoint with a scope.
  - Draw time stayed where it was: 0.2-1.7 ms per screen over the 19 screens
    the dump renders.
- **Measured against the website** ([baseline](../benchmarks/baseline-2026-10-20.md)), same machine and session, median of 10 runs:
  - Fresh content: 0.27–0.33 s per step in the reader vs 2.1–2.7 s on the website.
  - Requests: 1 per screen vs 130–190.
  - Saved copies appear in about 1 ms.
  - The reader's own work (parse, update, draw) takes 1–3 ms; the rest is Canvas's response time.
  - All three build-plan performance goals are met.
- **Downloads**, measured live: a 908 KB PDF in 1.1 s through the full redirect chain, saved, linked and marked as downloaded from the internet.
- **Announcements live check (2026-10-21).** Rust read all 1,380 announcements across 17 courses and opened one detail in each of the 13 nonempty courses. Median list refresh: 237 ms. The 674-item course took 5.43 s across seven pages, 205 KB; cached lists remain visible while refreshing. Counts match the earlier probe; no account content is included in the report.
- **Checks.** 277 Rust tests (Windows) and 32 Python tests pass. `canter --smoke` re-verifies live reads, file access and one download; `canter --dump DIR` renders each screen headlessly, including the Grades tab and the instructor's feedback, and downloads one file into a scratch library.

- **Images in the terminal** (prototype). Images embedded in assignment, page
  and announcement text are drawn where the text has them, from the copy the
  library already downloaded, as half blocks in a box that keeps the aspect
  ratio, sits in the middle of the text column and reflows with the width. An assignment's images are downloaded when
  it is opened, so they appear on their own; a page's and an announcement's are
  not, so until you download them (`D`, or `d` on one) the text keeps its `[n]`
  marks. A finished download drops the wrapped text so the picture appears at
  once, rather than at the next refresh or resize. `CANTER_IMAGES=off` turns them off and
  leaves the `[n]` marks unchanged. Sixel and the kitty protocol are not drawn.

## Known issues and limits

- **The warm-up costs about 68 reads a run.** 17 courses means 51 reads for the assignment, announcement and tab lists, plus 3 page lists and 9 file listings where the tabs allow them, plus the 5 Inbox folders: roughly 14 s of background traffic at two at a time, on every run rather than only the first. Saved copies make the box useful at once, so this is never waited on, but it is real traffic. If it proves too much, warming only on the first `/` is a small change.
- **Only titles are searched**, plus the course or folder a thing sits in. Message bodies, assignment descriptions and page text are not, so the box finds "Lab 2" but not a phrase inside it.
- **A result is as fresh as the last read of its list.** Until the warm-up reaches a course, its results come from the saved copy, which may be days old. The box says how far it has got, but an individual row does not say which side of that line it came from.


- **Sessions don't last long.** Two data points so far: both expired within a day (under 22 h), and each needed a password and Duo again.
- **Sign-in needs a browser, not Python.** The reader is one Rust binary and drives Microsoft Edge (or Chrome) itself; only the `--python` fallback needs the project's Python environment and Playwright. Edge is still required, and the Rust path is unverified against UF live.
- **Windows only** for now: session protection uses DPAPI.
- **Order of undated assignments** differs from Canvas in 1 of 17 courses (cosmetic).
- **Back re-reads the list.** Returning to a list from a detail re-reads it, per the approved "fresh on every entry" rule. This costs one request per Back.
- **Git Bash** may print sign-in progress as separate lines instead of one updating line. PowerShell and Windows Terminal show the spinner properly.
- **The baseline is narrow:** one machine, one evening, 10 runs, with the browser headless. Good for medians, not for tail latency.
- **Images in the terminal are a prototype.** Embedded images are drawn as
  half blocks (`▀`, two pixels per cell) where the text has them. Sixel and the
  kitty graphics protocol are not drawn, because placing escape sequences
  inside a `ratatui` frame means reserving cells, repainting fully and
  suppressing the picture under every overlay; nor is the terminal asked what
  it supports, since that answer is only worth its cost once they are drawn.
  `docs/architecture.md` records what is missing. Decoding happens in the same
  pass that wraps the text, so it is bounded (400 ms per page) and cached per
  version and width, but dragging the window narrower re-decodes at each width;
  a cache keyed by file and box size is the fix if that proves annoying. `CANTER_IMAGES=off` restores the plain `[n]`
  marks. Other image files still open in an outside app.
- **Locked and hidden files.** Files Canvas marks as locked are not downloaded (one past lab here). In 8 of 17 courses the instructor hides Files, so the Files tab is empty there; attachments linked from assignments still work. Two linked files return 403 and can only be opened in the browser.
- **The grade column is one read per course per run.** With the switch on, entering the course list reads the grades of the courses listed once in a run (a saved total shows at once and is not re-read); a course's own Grades tab is always fresh. So a grade changed mid-session shows on the Grades tab before the course list.
- **Grades are read, never written.** canter shows what Canvas computes and does not do its own what-if arithmetic.
- **The comment marks depend on one read** (`/courses/:id/students/submissions` with `student_ids[]=self`). Where a course refuses it, grades still show but no row is marked. Checked live on 2026-10-22: all 17 courses answered, median 253 ms.
- **Out of scope for now:** discussions (links to them open in the browser). Quizzes and assessments are permanently out of scope.
- **Website-URL submissions are untested live.** No assignment in this account uses `online_url`, so that path has only been exercised offline against a scripted transport. It needs one live check once an assignment of the type appears.
- **Feedback drawn on a PDF stays in the browser.** Canvas's document viewer annotations are not in the submission API, so the reader says which file carries them and offers `o`; it never renders them as text. The same holds for audio and video comments.

## What's next

**Now: version 2, submissions** (milestones 9–10 above). The first live upload will use an assignment and files you choose in your UF account. A survey of all 161 assignments in your 17 courses shaped the plan:

- **How work is handed in:** file upload 63, Canvas quizzes 52, external tools 34, text entry 18.
- **What assignments link to:** 63 distinct Canvas files (32 PDFs, 9 images, 6 Word files), 18 Canvas pages, 28 modules and 49 outside sites.

Quizzes and assessments are permanently out of scope.

After version 2, the remaining version 1 milestones follow. The baseline shows that almost all remaining time is Canvas's network response (≈260–410 ms per read), so the next wins come from overlapping or avoiding reads rather than from faster local code.

1. **Milestone 4.** Decide per resource whether anything beyond the three saved copies is worth reusing. For example, the course list (21 KB, rarely changes) could be shown from its saved copy and refreshed less often. Each change needs your approval under the freshness rules.
2. **Milestone 5.** Prefetch an assignment's detail after its row stays selected for about 150 ms, under the approved 2-second freshness rule. That could hide most of the ≈270 ms detail read. Measure hits and wasted requests with prediction on and off, using `--bench` and `--diag`.
3. **Session lifetime.** Run with `--diag` over several days to learn how long sessions last, and whether choosing "remember this device" in Duo skips the push.
4. **Milestone 6.**
   - Hardening: throttling, resizing, terminal restore on errors, install instructions.
   - Hardening the Rust sign-in after its first live runs (see [authentication.md](authentication.md)).

## How to check the current state

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1 -Tasks "pytest build test smoke"
target\release\canter.exe --dump .tools\dev
target\release\canter.exe --bench 10
.venv\Scripts\python.exe -m tools.browser_bench --runs 10
```

`--smoke` writes `benchmarks/private/smoke.json` (metadata only). `--dump` writes the rendered screens as text. The two benchmarks write `benchmarks/private/tui-bench.json` and `browser-bench.json`.

- **Announcement comment-lock fix (2026-10-21).** `locked_for_user` may mean replies are closed while the message remains readable. Render the server-returned body and use `lock_info.can_view` for links. Regression coverage includes closed-comments bodies/links and replay of private saved responses without logging content. The earlier live check verified response IDs but did not verify body rendering; the render regression now covers that missed case.
