#!/usr/bin/env python3
"""Render scripts/tickets.json into docs/TICKETS.md.

    python3 scripts/render-tickets.py

Keeps the manifest and the browsable doc in sync — edit tickets.json, re-run,
commit both.
"""

import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "scripts", "tickets.json")
TARGET = os.path.join(ROOT, "docs", "TICKETS.md")

STATUS = {True: "done", False: "open"}

# Milestone order matters: the doc is grouped by it.
MILESTONE_ORDER = [
    "v0.1 — Core loop",
    "v0.2 — AI round-trip",
    "v0.3 — Everyday editing",
]

tickets = json.load(open(MANIFEST))
num_of = {t["key"]: i for i, t in enumerate(tickets, start=1)}

lines = [
    "# Tickets",
    "",
    "Source of truth for this file is [`scripts/tickets.json`]"
    "(../scripts/tickets.json). Edit that, then re-run "
    "`python3 scripts/render-tickets.py`.",
    "",
    "Issue numbers below assume creation order on a fresh repository (1..%d)."
    % len(tickets),
    "",
]

by_ms = {}
for t in tickets:
    by_ms.setdefault(t["milestone"], []).append(t)

for ms in MILESTONE_ORDER:
    group = by_ms.get(ms, [])
    if not group:
        continue
    done = sum(1 for t in group if t["done"])
    lines += [
        "## %s  —  %d/%d closed" % (ms, done, len(group)),
        "",
        "| # | Ticket | Labels | Blocked by | Status |",
        "| -:| ------ | ------ | ---------- | ------ |",
    ]
    for t in group:
        n = num_of[t["key"]]
        blocked = ", ".join("#%d" % num_of[b] for b in t["blocked_by"]) or "—"
        labels = " ".join("`%s`" % l for l in t["labels"])
        lines.append(
            "| %d | [%s](#%d-%s) | %s | %s | %s |"
            % (n, t["title"], n, t["key"], labels, blocked, STATUS[t["done"]])
        )
    lines.append("")

lines += ["---", "", "## Detail", ""]
for t in tickets:
    n = num_of[t["key"]]
    lines += [
        "### #%d — %s" % (n, t["title"]),
        "",
        "`%s` · milestone **%s** · **%s**"
        % (t["key"], t["milestone"], STATUS[t["done"]]),
        "",
        t["body"].strip(),
        "",
    ]
    blockers = [x["key"] for x in tickets if t["key"] in x["blocked_by"]]
    if t["blocked_by"] or blockers:
        rel = []
        if t["blocked_by"]:
            rel.append("Blocked by " + ", ".join("#%d" % num_of[b] for b in t["blocked_by"]))
        if blockers:
            rel.append("Blocks " + ", ".join("#%d" % num_of[b] for b in blockers))
        lines += ["*%s.*" % " · ".join(rel), ""]

lines += ["---", "", "## Dependency graph", "", "```text"]
for t in tickets:
    if t["blocked_by"]:
        for b in t["blocked_by"]:
            lines.append("  #%-2d %-22s <-- #%-2d %s"
                         % (num_of[t["key"]], t["key"], num_of[b], b))
lines += ["```", ""]

os.makedirs(os.path.dirname(TARGET), exist_ok=True)
with open(TARGET, "w") as f:
    f.write("\n".join(lines))
print("wrote %s (%d tickets)" % (TARGET, len(tickets)))
