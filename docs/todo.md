# To-do list

Agreed on 2026-10-22. Order is roughly the order of work. Decisions already
made are recorded here so they are not re-litigated.

**State on 2026-10-22, evening.** Items 1, 2, 3 and 5 are built, merged and
offline-tested, and items 1 and 2 are verified against the account. Item 4
(sign-in in Rust) is built and merged but is only verified as far as UF's
login form: the password, Duo, the cookie capture and each failure still need
one live run. What remains unverified live is listed under "Live checks still
owed" at the end.

## Decided

- **Sign-in.** Rust drives Edge itself (no Python, no Playwright) for UF, with a
  separate small visible sign-in window as the fallback and as the option for
  other Canvas sites. The window opens on its own, not as a tab in a browser
  the user already has open, is about the size of a phone screen, and closes
  itself once sign-in finishes.
- **Home screen `Tab`.** `Tab` cycles Courses → Calendar → Inbox, and
  `Shift+Tab` goes back the other way, as it already does everywhere else.
- **Current grade on the course list.** One switch for all courses, in the
  `Space` menu, saved locally like hidden courses.
- **New feedback.** An assignment whose instructor comments have not been seen
  in canter gets a dot on the Grades tab. "Seen" is tracked locally; Canvas
  is never told.

## 1. Course tabs: grades and pages

- Grades tab, shown only where Canvas shows it:
  - course total and letter grade at the top, left out where the instructor
    hides it;
  - assignments in Canvas's groups, with each group's weight;
  - score out of points, plus late, missing and excused;
  - a dot on rows with instructor comments not yet seen here.
- Current grade on the home course list, behind one switch for all courses.
- Pages tab: every page in the course with when it was updated, opening in
  canter.

## 2. Assignment page: feedback and submissions

- Instructor feedback for each attempt: comments, with their attached files
  saved to `Assignment Files/<assignment>/Feedback`; rubric scores per
  criterion; "open in browser" for comments drawn on PDFs.
- Text-entry submissions, typed in canter or read from a file, with the
  same review screen and explicit confirmation as file uploads.
- Website-URL submissions, same flow. No assignment in this account uses the
  type, so it can only be tested offline until one appears.

## 3. Inbox

- Inbox tab on the home screen, read-only first: all folders (Inbox, Unread,
  Starred, Sent, Archived); conversations read in canter with their
  attachments going to the library; "read" tracked only in canter; saved
  copies capped at the 100 newest conversations and 100 messages in total.
- Replies and new messages after that, with a review screen and explicit
  confirmation.

## 4. Sign-in in Rust

As decided above. Its code does not overlap with the screens.

## 5. Images in the terminal

Images in assignment and page text: blurry half-block images by default,
sharper formats where the terminal supports them, behind a setting that turns
them off.

## Later or tabled

Discussions, syllabus, People, module progress (tabled), porting to macOS and
Linux (after everything else), saved-copy reuse and prefetching (build-plan
milestones 4 and 5), and the non-UF sign-in live check (milestone 9).

## Live checks still owed

- **Sign-in in Rust.** `--logout` then `--login`, then `--smoke`; the same with
  `--login --window`; then `CANTER_SIGNIN=python` to confirm the old helper
  still runs. Nothing past UF's form has been exercised.
- **A text-entry submission** to an assignment of your choosing (18 here take
  the type).
- **A website-URL submission.** No assignment in the account uses `online_url`,
  so it cannot be checked until one appears.
- **Sending from the Inbox** (a reply and a new message to teachers). Reading
  every folder, opening a conversation and leaving Canvas's read state alone is
  checked (`cargo test --release live_inbox -- --ignored`).
- **A feedback file.** No instructor comment in the account has an attachment,
  so `Assignment Files/<assignment>/Feedback` has not taken a real file.
- **Images on screen.** Only you can judge the half-block drawing; the tests
  check the cells, not how it looks.
