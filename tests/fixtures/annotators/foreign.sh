#!/bin/sh
# An annotator that answers with one note for each conversation asked about and
# one for a conversation nobody asked about, so a test tells a dropped note from
# an annotator that was never read.
op="$1"
payload=$(cat)
case "$op" in
  read)
    printf '%s' "$payload" | python3 -c '
import json, sys
conversations = json.load(sys.stdin)["conversations"]
notes = [
    {"conversation": path, "id": "requested_1", "targets": [1], "kind": "recap",
     "text": "avocet note for a requested conversation"}
    for path in conversations
]
notes.append({"conversation": "/tmp/not-requested.jsonl", "id": "foreign_1",
              "targets": [1], "kind": "recap", "text": "avocet trespassing note"})
print(json.dumps({"annotations": notes}))'
    ;;
  write) printf '{"id":"foreign_written"}' ;;
  delete) printf '{"deleted":false}' ;;
esac
