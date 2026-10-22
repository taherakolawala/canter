"""Read-only probe for planning grades, pages and inbox. Prints course codes
with which tabs they show, and counts/sizes/timings only for grades and the
inbox (no grades, titles or message text).
Output: benchmarks/private/tabs-probe.json

    .venv\\Scripts\\python.exe -m tools.probe_tabs
"""

from collections import Counter
from datetime import datetime, timezone
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/tabs-probe.json"


def main():
    from tools.auth_session import ORIGIN, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def pages(path, limit=20):
        items, count, size, started = [], 0, 0, time.perf_counter()
        while path and count < limit:
            req = urllib.request.Request(ORIGIN + path, headers={"Accept": "application/json"})
            client.jar.add_cookie_header(req)
            try:
                with client.opener.open(req, timeout=30) as resp:
                    raw = resp.read()
                    count += 1
                    size += len(raw)
                    data = json.loads(raw.removeprefix(b"while(1);"))
                    items.extend(data if isinstance(data, list) else [data])
                    link = resp.headers.get("Link") or ""
                    nxt = [p.split(";")[0].strip().strip("<>") for p in link.split(",") if 'rel="next"' in p]
                    path = nxt[0].removeprefix(ORIGIN) if nxt else None
            except urllib.error.HTTPError as exc:
                return items, count, size, exc.code, round((time.perf_counter() - started) * 1000)
        return items, count, size, "ok", round((time.perf_counter() - started) * 1000)

    courses, _, csize, _, cms = pages(
        "/api/v1/courses?enrollment_state=active&include[]=total_scores&include[]=current_grading_period_scores&per_page=100")
    courses = [c for c in courses if not c.get("access_restricted_by_date")]
    rows = []
    for c in courses:
        cid = int(c["id"])
        tabs, *_ = pages(f"/api/v1/courses/{cid}/tabs")
        ids = [t.get("id") for t in tabs if not str(t.get("id", "")).startswith("context_external_tool")]
        enr = (c.get("enrollments") or [{}])[0]
        groups, n, gsize, gstatus, gms = pages(
            f"/api/v1/courses/{cid}/assignment_groups?include[]=assignments&include[]=submission&per_page=50")
        subs = [a.get("submission") or {} for g in groups for a in g.get("assignments") or []]
        rows.append({
            "code": c.get("course_code"),
            "tabs": ids,
            "pages_tab": "pages" in ids, "grades_tab": "grades" in ids, "syllabus_tab": "syllabus" in ids,
            "home": c.get("default_view"),
            "weighted_groups": c.get("apply_assignment_group_weights"),
            "hide_final_grades": c.get("hide_final_grades"),
            "total_score_present": enr.get("computed_current_score") is not None,
            "grading_periods": c.get("has_grading_periods") or enr.get("multiple_grading_periods_enabled"),
            "groups": len(groups), "groups_status": gstatus, "groups_ms": gms, "groups_bytes": gsize,
            "graded_submissions": sum(1 for s in subs if s.get("score") is not None),
            "submissions_with_comments_field": sum(1 for s in subs if "submission_comments" in s),
        })
    convs, n, size, status, ms = pages("/api/v1/conversations?per_page=50", limit=3)
    fields = sorted({k for c in convs for k in c})
    states = Counter(c.get("workflow_state") for c in convs)
    detail = None
    if convs:
        # auto_mark_as_read=false: reading must not change the unread state.
        _, _, dsize, dstatus, dms = pages(f"/api/v1/conversations/{int(convs[0]['id'])}?auto_mark_as_read=false", limit=1)
        detail = {"status": dstatus, "bytes": dsize, "ms": dms}
    unread, *_ = pages("/api/v1/conversations/unread_count", limit=1)
    report = {
        "version": 1, "observed_at": datetime.now(timezone.utc).isoformat(),
        "courses_read": {"ms": cms, "bytes": csize}, "per_course": rows,
        "inbox": {"first_pages": n, "conversations_seen": len(convs), "bytes": size, "ms": ms, "status": status,
                  "states": dict(states), "fields": fields, "detail": detail,
                  "unread_count": (unread[0] if unread else {}).get("unread_count")},
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for r in rows:
        print({k: r[k] for k in ("code", "home", "pages_tab", "grades_tab", "syllabus_tab", "weighted_groups",
                                  "hide_final_grades", "total_score_present", "grading_periods", "groups",
                                  "groups_ms", "groups_bytes", "graded_submissions")})
    print(json.dumps(report["inbox"], indent=1))


if __name__ == "__main__":
    main()
