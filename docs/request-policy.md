# Request policy ledger

Every network read the reader makes is listed here with its purpose, trigger, and freshness rule. So is every request the Canvas web page made for the same task, with the reason it's omitted from the reader. Nothing is fetched speculatively yet; prediction is Milestone 5. The one automatic download (images in an assignment the user opened) is listed under file downloads.

Evidence:
- The web page's requests come from the analysed HAR (`benchmarks/initial-findings.md`), one assignment-detail page.
- The reader's measurements come from `tuicanvas --smoke` (`benchmarks/private/smoke.json`, kept local). Live figures are filled in under "Measurements" once collected.

## Reads the reader performs

| Read | Request | Fields used | Trigger | Freshness / reuse |
| --- | --- | --- | --- | --- |
| Course list | `GET /api/v1/courses?enrollment_state=active&include[]=term&include[]=favorites&per_page=100` (all pages) | id, name, course_code, term name, is_favorite, access_restricted_by_date | Each entry to the course list, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. Courses restricted by date are hidden. |
| Assignment list | GraphQL `TuiAssignments`: `course.assignmentsConnection(first: 100)` with `_id name dueAt unlockAt lockAt pointsPossible submissionTypes htmlUrl position lockInfo{isLocked}` and `submissionsConnection(first: 1, filter: {states: [unsubmitted, submitted, pending_review, graded, ungraded]})`, all cursor pages. Falls back to `GET /api/v1/courses/:id/assignments?include[]=submission&order_by=due_at&per_page=100` if Canvas rejects the query. | The fields named in the query (no descriptions) | Each entry, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. A failed page fails the whole list rather than showing a partial one. Sorted by due date (undated last), then case-insensitive name, then id. |
| Assignment detail | `GET /api/v1/courses/:id/assignments/:id?include[]=submission` | The list fields plus description (HTML), lock_explanation, allowed_attempts, and the rubric Canvas already sends with the assignment (`rubric`: criterion id, description, long_description, points; `rubric_settings`: title, points_possible) | Each entry; `r` | Saved copy shown with Syncing; fresh read on every entry. The rubric needs no include and no extra request; it supplies the criterion names and points the feedback section prints beside the scores. |
| Past submissions and the instructor's feedback | `GET /api/v1/courses/:id/assignments/:id/submissions/self?include[]=submission_history&include[]=submission_comments&include[]=rubric_assessment` | Attempt numbers, dates, type, status, grade when visible, submitted text/URL and file names/sizes; each attachment's `canvadoc_document_id`/`preview_url`; each comment's id, author_name, created_at, comment, attempt, attachments and media_comment; the rubric assessment's points and comments per criterion id | With the assignment page, when the copy on hand shows something was submitted; `r` | One fresh request per visit. The comments and the rubric scores are includes on the attempt read rather than a second request, because the same submission object carries all three and the page already pays for it. Kept in memory only and never saved to disk, as attempts may contain the student's work. Comments Canvas carries no text for (a recording, or marks drawn in its document viewer) are labelled as such and offer the browser; nothing is invented for them. |
| Announcement list | GraphQL `TuiAnnouncements`: `discussionsConnection(first: 100, filter: {isAnnouncement: true})`, all cursor pages (at most 20) | id, title, posted/delayed/created dates, author short name, participant read state, reply count; no message bodies | Each entry, including Back; `r` | Saved copy with Syncing; every visit re-reads. Sort all metadata by posting date, then delayed date, then creation date (missing dates last). The connection is created-time ordered, so sorting just its first page cannot guarantee the latest published items. Schema rejection falls back to the paginated REST discussion list with `only_announcements=true`. Other failures retain an Outdated saved copy. Missing/repeated cursors and page-limit exhaustion fail the entire refresh. |
| Announcement detail | `GET /api/v1/courses/:id/discussion_topics/:id` | Summary fields, message HTML, attachments, locked-for-user/reason, is-announcement | Each entry; `r` | Saved copy with Syncing. Validate the returned ID/type. Render the message returned by Canvas even when comments are closed; Canvas itself substitutes an access explanation when reading is restricted. `lock_info.can_view` controls link/attachment visibility, not `locked_for_user`. Reading marks it read locally, without a Canvas write; replies remain in the browser. |
| Files attached to an instructor comment | `GET /api/v1/files/:id`, followed by the existing download flow if needed | Current file metadata | Only `Enter`, `d` or `D` on the feedback section of an assignment | The comment carries the attachment inline, but its metadata is read again on each choice, as announcement attachments are, so a stale address or version never decides what is on disk. Ids that are not plain Canvas ids are dropped before any request. Files land in `Assignment Files\<assignment>\Feedback`. No speculative reads: nothing is fetched by showing the feedback. |
| Files chosen from announcements | `GET /api/v1/files/:id`, followed by the existing download flow if needed | Current file metadata | Only `Enter`, `d`, or `D` on announcement files (including inline images) | Refresh metadata on each choice; a recent in-flight read may be shared. Validate the current version before reusing a local copy. No speculative file reads or automatic image downloads for announcements. |
| Inbox folder list | `GET /api/v1/conversations?scope=unread\|starred\|sent\|archived&per_page=100` (no `scope` for Inbox; at most 10 pages) | id, subject, workflow_state, last_message, last_message_at, last_authored_message_at, message_count, starred, context_name, participant ids and names; no message bodies | Each entry to the home screen, including Back; moving to another folder with `←`/`→`; `r` | Saved copy shown with Syncing; fresh read on every entry, and one read per folder shown. Sorted by latest activity (last message, else last authored message), newest first; the saved copy keeps the 100 newest conversations. |
| Conversation detail | `GET /api/v1/conversations/:id?auto_mark_as_read=false` | Summary fields plus each message's id, created_at, body, author id, generated flag and attachments | Each entry; `r` | Saved copy with Syncing. Validate the returned id. **`auto_mark_as_read=false` is required:** without it this endpoint marks the conversation read on Canvas as a side effect. It is the only read in the reader that could change server state, and the parameter is part of the request, not an option. Read state is kept locally instead; nothing is written to Canvas by reading. Messages are sorted newest first and the saved copy keeps the 100 newest, with a 100-message budget across all saved conversations. |
| Files chosen from conversations | `GET /api/v1/files/:id`, followed by the existing download flow if needed | Current file metadata | Only `Enter`, `d`, or `D` on a conversation's files (including inline images) | Refresh metadata on each choice; a recent in-flight read may be shared. Nothing about a conversation's attachments is read until one is chosen, so their sizes are unknown until then. No automatic image downloads for conversations. |
| Course files | `GET /api/v1/courses/:id/folders?per_page=100` (at most 20 pages) and `GET /api/v1/courses/:id/files?per_page=100` (at most 50 pages), run together | Folders: id, name, full_name, parent_folder_id, locked/hidden for user. Files: id, display_name, filename, size, content-type, updated_at, modified_at, url, folder_id, locked_for_user, lock_explanation | Each entry to a course's Files tab, including Back to it from outside the tab; `r`. Moving between folders and searching read nothing. | Saved copy shown with Syncing; fresh read on every entry. A 403 on the files list means the instructor hid Files: the tab says so and the folders are not shown. |
| File details | `GET /api/v1/files/:id` | As for the files list | For each file linked from an assignment, each time that assignment's detail is read; also when a linked file is opened and its details are unknown | Saved copy kept for the list's display; fresh read with every detail read. Downloads wait for an in-flight read. |
| Grades: groups and scores (median 461 ms, 2.6 s worst case over 17 courses) | `GET /api/v1/courses/:id/assignment_groups?include[]=assignments&include[]=submission&exclude_response_fields[]=description&exclude_response_fields[]=rubric&per_page=50` (at most 10 pages) | Group id, name, position, group_weight; each assignment's id, name, due date, points, omit_from_final_grade and the student's own submission (score, grade, late, missing, excused, state) | Each entry to the Grades tab, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. Descriptions and rubrics are excluded because the tab does not show them. |
| Grades: course total | `GET /api/v1/courses/:id?include[]=total_scores&include[]=current_grading_period_scores` | The student enrolment's computed_current_score and computed_current_grade, and apply_assignment_group_weights | With the groups above | As above. Canvas leaves the computed scores out when the instructor hides final grades, so an absent total is shown as "not shown here" rather than as zero. |
| Grades: instructor comments (200 in all 17 courses, median 253 ms) | `GET /api/v1/courses/:id/students/submissions?student_ids[]=self&include[]=submission_comments&per_page=100` (at most 10 pages) | Per submission: assignment id, user id, and each comment's author id and date. No comment text is read here | With the groups above | As above. The marks are extra, so a course that refuses this read (403) keeps its grades and simply shows no marks; an expired session is still reported. Only comments by someone other than the student count. |
| Page list | `GET /api/v1/courses/:id/pages?sort=updated_at&order=desc&per_page=100` (at most 20 pages) | Page URL name, title, last update, front-page flag, locked-for-user, html_url. No page text | Each entry to the Pages tab, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. Pages whose URL name is not a safe slug are left out rather than shown as rows that cannot be opened. A page's text is read only when it is opened (the existing page read). |
| Grades on the course list | The same three reads, per course | As above | Only while the course list is set to show grades (`Space` then `g`), and only for courses listed whose grades this run does not have; a saved total is shown without a read | The switch is local and saved; turning it off reads nothing, and refreshing the home screen does not re-read grades. |
| Profile | `GET /api/v1/users/self/profile` | id | Only when the saved session lacks an account id | Not cached; scopes saved copies to the account. |

## Requests the reader sends

Sending is never a consequence of reading. Each request below is made only from a review screen that shows the recipients, the subject and the body exactly as they will be sent, and only when the user presses the key that confirms it (`y`). Esc at either step discards the draft and sends nothing.

| Request | Fields sent | Trigger | Rules |
| --- | --- | --- | --- |
| Reply to a conversation | `POST /api/v1/conversations/:id/add_message` with `body` | `R` on an open conversation, then `Ctrl+D` to review, then `y` | Only `body` is sent: no `workflow_state` and nothing else that would change the conversation's read state. The conversation must have been read from Canvas in the last two minutes, so the review shows the thread that is really there. Canvas must answer with the same conversation id and at least one message, or the reply is reported as failed rather than assumed sent. On success the conversation and the folder list are read again. |
| New message to a course's teachers | `POST /api/v1/conversations` with `recipients[]=course_<id>_teachers`, `subject`, `body`, `context_code=course_<id>`, `mode=sync` | `N` on the course list, then `Ctrl+D` to review, then `y` | Recipients are named by Canvas context code, so no recipient-search read is added and the review can state exactly who receives it. Each teacher gets a separate conversation, which the review says. Canvas must answer with at least one created conversation. Subject at most 255 characters, body at most 8,000; an empty or oversized message is refused before the review and again before the request. |

## The search warm-up

`/` and `Ctrl+K` search what the reader already has; searching itself sends nothing. For that to be worth anything the lists have to be in hand, and nothing reads a course the user never opens, so the reader fetches them in the background.

| Request | Trigger | Rules |
| --- | --- | --- |
| Each course's assignment list, announcement list and tab list; then its page list and file listing where its tabs say it has them; and the five Inbox folders | The course list arriving, once per run | Queued, not fired at once: at most two warm-up reads are in flight, out of the four the executor allows, so the screen the user is on is never left queued behind the warm-up. A list already read, already being read, or available as a saved copy is skipped. A failure drops that list from the search and the queue carries on. Hidden courses are included, because hiding one tidies the course list rather than saying to forget the course. |

**What it costs here.** 17 courses means 51 reads for the assignment, announcement and tab lists, plus 3 page lists and 9 file listings where the tabs allow them, plus the 5 Inbox folders: **68 reads**, about 14 seconds of background traffic at two at a time. That is the first run only. A list already on disk is not read again, so from the second run the warm-up is a disk walk and asks Canvas for nothing. The price of that is staleness: a course not opened for a week is searched as it was a week ago, and only opening it refreshes what the search knows.

This is the one place the reader asks Canvas for something the user did not ask for, so it is bounded on every side: once per run, nothing re-read, no second attempt after a failure, and off entirely unless a person is driving the reader (the headless walk-through, the benchmark and the tests state exactly what they fetch). Every list it reads is one the user could have reached by opening the course, and it is read exactly the way opening the course would read it. The palette shows how far it has got rather than presenting a partial index as the whole account.

## File downloads

| Download | Request | Trigger | Rules |
| --- | --- | --- | --- |
| A file the user chose | The file's `url` from its details (`/files/:id/download?download_frd=1&verifier=…` on the Canvas origin), then Canvas's redirects: `*.canvas-user-content.com` → `inst-fs-*.inscloudgate.net` → `cdn.inst-fs-*.inscloudgate.net` | `Enter` (download if needed, then open), `d`, `D` (all files of a folder and its subfolders, or of an assignment) | Not downloaded again while the saved copy matches Canvas's version (modified time and size). Locked files are not requested. |
| Images in an assignment | Same | When the assignment's detail is read (which the user started by opening it), after the image's details arrive | Stored in the assignment's folder; removed after a week unused unless the user opened or downloaded it |

- **Credentials.** Canvas cookies go only to the Canvas origin, on the first hop. Every later hop must be HTTPS on port 443 to one of Canvas's two storage domains and gets no cookies (its address is signed). Anything else stops the download.
- **Limits.** At most 2 downloads at once, separate from the 4 read slots; at most 6 redirects; at most 2 GB per file. The body streams to `.tuicanvas\partial` and is installed only when complete and the length matches.
- **After download.** The file gets the "downloaded from the internet" mark (Zone 3) that browsers add. It opens in its default app; programs and scripts are shown in File Explorer instead of being opened.

## Writes the reader makes

These are the only requests that change anything on Canvas. Each one is reached from a review screen that shows exactly what will be sent, and happens only after the user presses `y` there; no other key sends anything, and nothing is retried on its own.

| Write | Request | What is sent | Trigger | Rules |
| --- | --- | --- | --- | --- |
| File upload | `POST /api/v1/courses/:c/assignments/:a/submissions/self/files` per file, then the signed upload Canvas names, then `POST .../submissions` with `submission[submission_type]=online_upload` and `submission[file_ids][]` | The chosen files' names, sizes and contents | `s` on an assignment, then `y` on the review screen | Each file's size is checked against the review again before it is sent. Verified afterwards by re-reading the submission and matching every file id. |
| Text entry | `POST /api/v1/courses/:c/assignments/:a/submissions` with `submission[submission_type]=online_text_entry` and `submission[body]` | The reviewed text, byte for byte, typed in TUICanvas or read from a UTF-8 file the user named (at most 100,000 characters) | `t` on an assignment, then `y` | Verified by re-reading the submission: the type must match and the stored body must not be empty. Canvas sanitises the HTML it stores, so the text is not compared character by character. |
| Website URL | The same `POST`, with `online_url` and `submission[url]` | The reviewed address. An address typed without a scheme gets `https://`, and the review screen shows the result, so what is confirmed is what is sent. | `u` on an assignment, then `y` | Only `http` and `https`, no whitespace or control characters, a host with a dot, at most 2,000 characters. Verified by re-reading the submission; when Canvas records a different address, its version is reported. |

- **No assignment in this account uses `online_url`,** so that write has been exercised only against a scripted transport offline. It needs a live check once an assignment of the type appears.
- Ids are checked with `valid_id` before they reach any path. The CSRF token goes with these POSTs as with any Canvas write.
- A failed write never retries by itself. The message says to refresh before trying again, because Canvas may have recorded the attempt.

Shared rules:
- **Freshness.** A response may satisfy an access only if its request began no more than 2 s before that access (an in-flight read is promoted, not duplicated). Explicit refresh always issues a new read. A response from an older request never replaces data requested later.
- **Failures.**
  - The displayed copy stays on screen and is labelled Outdated.
  - An authentication failure (401 "authorization required", redirect to `/login`, or CSRF rejection) asks the user to sign in.
  - A permission failure (401 "unauthorized" or 403) is shown as no access, without prompting a sign-in.
  - Rate limiting (403 "Rate Limit Exceeded" or 429) backs off and retries twice.
- **Requests.**
  - At most 4 network operations at once, over one pooled HTTPS client.
  - GraphQL is used only for the reviewed, read-only list query above.
  - Canvas cookies only: identity-provider cookies are never sent to Canvas.
  - The CSRF token is sent only on POSTs: the GraphQL read and the submission writes above.
  - Paths are restricted to `/api/v1/` and `/api/graphql`, and pagination links must stay on the same origin.
- **Saved copies.** Stored for the course list, assignment lists, assignment details, course file listings, file details, Inbox folder lists and conversations, scoped by Canvas origin and account id. Logout deletes them. Downloaded files are not saved copies: they stay in the file library until you delete them (automatic images: a week unused).

## Web-page requests omitted from the reader

| Observed request (assignment page) | Purpose | Decision for the reading workflow |
| --- | --- | --- |
| Assignment page HTML (875 ms) | Server-rendered shell that bootstraps the JS app | Replaced by the JSON reads above |
| ~140 scripts, 4 stylesheets, fonts, theme images | Web UI | Not needed by a terminal UI |
| GraphQL `GetStudentAssignment` (2.2 KB, started ~3.3 s after navigation) | Assignment, rubric, submission and draft for the web submission view | Replaced by the documented REST detail read, which supplies the displayed fields without needing a submission relay id. Its rubric, drafts and media fields are deferred. A minimal GraphQL detail query saved only 30–70 ms (median 183 ms vs 213–252 ms), not enough to justify leaving the documented read. |
| GraphQL `ExternalTools` | LTI tools offered for submission | Omitted: no submission in v1 |
| GraphQL `NextSubmission` | Submission history excluding the current attempt | Replaced by the documented REST single-submission read with `include[]=submission_history`, requested only when the student presses `h` |
| GraphQL `GetUserGroups` | The user's groups, for group assignments | Deferred until group assignments are displayed specially |
| `module_item_sequence` (3 requests) | Previous/next module navigation | Omitted: module navigation is out of scope for v1 |
| `lti_apps/launch_definitions` | LTI placements on the assignment view | Omitted: no LTI launches |
| `release_notes/unread_count` | Canvas "what's new" badge | Omitted |
| `pages/account-settings` (course page read) | Institution-specific page script | Omitted |
| `web-app-manifest`, thumbnails, telemetry endpoints | Browser and analytics | Omitted |

## Decisions from measurements

Live read-only runs of `tuicanvas --smoke` on 2026-10-16 (one account, 17 active courses, 157 assignments, Windows, home connection):

| Question | Result | Decision |
| --- | --- | --- |
| Do native reads work outside the browser? | Profile, courses, assignment lists and details all return 200 using the saved session | Native transport; no browser bridge |
| Assignment list: REST vs field-selected GraphQL | REST 944 KB total, median 520–980 ms per course (slowest 9.6 s). GraphQL 64–75 KB, median 290–440 ms (slowest 1.9 s). | Use GraphQL, with a REST fallback |
| Same data? | After requesting unsubmitted submissions explicitly (the filter's default hides them, and with them the **missing** flag), the two match on every assignment: ids, due dates, points, status, locks, submission types, links | Required before switching; met |
| Same order? | Canvas orders equal or undated due dates by name. A case-insensitive name tie-break reproduces REST order on 16/17 courses; the other differs only among undated assignments | Accepted as cosmetic |
| Assignment detail: REST vs GraphQL | Median 213–252 ms vs 183 ms; 3.9 KB vs 0.7 KB | Keep REST (documented; margin small) |
| Is CSRF needed for GraphQL reads? | Yes: without `X-CSRF-Token`, Canvas answers HTTP 422 "An error occurred." | Send it; treat a 422 on a GraphQL read as a session problem |
| Failure classes | No session → 401 "user authorization required"; course not enrolled → 404; rate-limit headers report 700 remaining, cost ≈0.1–0.3 per read | Classified as sign-in needed vs not found vs no access |
| Course Files listings (2026-10-21) | Folders list in all 17 courses; the files list is refused (403) in 8, where the instructor hid Files. Largest course: 133 files over 2 pages; folders at most 3 deep. One course's listing: 2 requests, 14 KB, about 310–340 ms. | Read both lists together; treat 403 on files as hidden |
| Files linked from assignments (2026-10-21) | 63 distinct across all courses: 59 readable, 1 locked, 2 refused (403), 1 not found. In one course, a past lab's file reads as locked ("This file is currently locked.") even though its details include an address. | Respect `locked_for_user`; show Canvas's reason; offer the browser for unreadable files |
| Download path (2026-10-21) | Canvas origin (cookies) → `canvas-user-content.com` → `inst-fs-iad-prod.inscloudgate.net` → `cdn.inst-fs-iad-prod.inscloudgate.net`, which serves the file (and supports ranges). A 660 KB PDF took about 1.0 s, a 908 KB PDF 1.1 s; sizes matched Canvas. Hard links work on the user's drive. | Manual redirects with the host rules above |
| Session lifetime | A session saved at 4:13 PM was rejected 22 h later; reuse within the same day worked | An expired session at startup opens sign-in directly |

## Open questions

- **Back navigation.** Re-entering a list after viewing a detail costs one list read under the current rule. Revisit only with the user.
- **Session lifetime.** One data point so far (< 22 h). Track it with `--diag` over several days.

## Announcements decision (2026-10-21)

The saved probes cover 17 courses and 1,380 announcements. In the largest course, REST fetched 3.54 MB over seven pages in about 24 seconds; much of this was message HTML that the list does not need. A 25-row GraphQL metadata sample was about 8.6 KB/329 ms versus REST's 182 KB/1,131 ms. These are probe measurements, not a promise about the full reader's latency.

GraphQL's first 50/100 nodes matched the REST top-25 set in these samples after local sorting, but that alone does not establish correctness for future delayed posts. The implementation reads all small metadata pages before sorting; full bodies remain on demand. Unknown participant state is shown as Unknown unless you have read the announcement locally. There is no all-courses polling/feed or mark-read mutation.

Reference: [Canvas Discussion Topics API](https://developerdocs.instructure.com/services/canvas/resources/discussion_topics). Local evidence: `benchmarks/private/announcements-probe.json`, `announcements-parity.json`; these contain counts/timings/schema only. The Rust live check is `cargo test --release live_announcements -- --ignored --nocapture` and writes `announcements-live.json` in the same private directory.

The Rust live check passed on all 17 courses (1,380 announcements, 13 detail reads): list median 237 ms, total list data 415,299 bytes. The largest course (674 items) needed seven requests and 5.43 s / 204,572 bytes. This is a remaining latency cost of fetching complete metadata; saved copies display throughout the refresh. Measurements are single runs, not a statistical benchmark.

Local announcement reads are stored separately from network snapshots in `local-announcement-reads.json`, scoped by origin/account and keyed by course/topic ID. An open detail with an available message and view permission marks Read; listing, failed/denied reads, and responses arriving after leaving the detail do not. Cached bodies displayed on entry count as read. Later server refreshes cannot reset this local indicator. Logout clears it with that account’s saved copies. No network calls are added.

## Inbox decision (2026-10-22)

The Inbox is a home-screen section beside the month calendar, with the folders Canvas offers: Inbox, Unread, Starred, Sent and Archived. Each folder is one metadata read; message bodies arrive only when a conversation is opened, as with announcements.

Read state is the reason the detail read is spelled out above. `GET /api/v1/conversations/:id` marks the conversation read on Canvas unless `auto_mark_as_read=false` is sent, and no other endpoint the reader uses touches read state. Opening a conversation here records it in `local-conversation-reads.json`, scoped by origin and account and keyed by conversation id, the same store shape as `local-announcement-reads.json`. An empty conversation and a failed or denied read are not marked. A later refresh still reports Canvas's own `workflow_state`, which the reader leaves alone, and shows the local mark alongside it. Logout clears it with that account's saved copies.

Attachments and inline images go to `Inbox Files/<conversation id>` in the file library and are downloaded only when chosen, so a conversation costs one read however many files it carries.

Reference: [Canvas Conversations API](https://developerdocs.instructure.com/services/canvas/resources/conversations). The live check is `cargo test --release live_inbox -- --ignored --nocapture`, which reads every folder, opens one conversation and re-reads the Unread folder afterwards to confirm Canvas's read state is unchanged. It reports counts and timings only.
