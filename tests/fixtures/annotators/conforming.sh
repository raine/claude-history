#!/bin/sh
# A conforming annotator: one note per requested conversation, a fixed id on
# write, a found delete. Every invocation appends its operation to
# $ANNOTATOR_CALL_LOG, so a test counts invocations per query, and a write
# copies its payload to $ANNOTATOR_WRITE_LOG, so a test reads the request the
# contract sent.
op="$1"
payload=$(cat)
[ -n "$ANNOTATOR_CALL_LOG" ] && echo "$op" >> "$ANNOTATOR_CALL_LOG"
case "$op" in
  read)
    printf '%s' "$payload" | python3 -c '
import json, sys
conversations = json.load(sys.stdin)["conversations"]
print(json.dumps({"annotations": [
    {"conversation": path, "id": "fixture_1", "targets": [2], "kind": "recap",
     "text": "pelican crossing from the fixture",
     "created": "2026-09-01T09:00:00+10:00",
     "modified": "2026-09-03T14:30:00+10:00"}
    for path in conversations
]}))'
    ;;
  write)
    [ -n "$ANNOTATOR_WRITE_LOG" ] && printf '%s' "$payload" > "$ANNOTATOR_WRITE_LOG"
    printf '{"id":"fixture_written"}'
    ;;
  delete) printf '{"deleted":true}' ;;
esac
