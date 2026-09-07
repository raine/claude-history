#!/bin/sh
# An annotator that acts on `replaces`, holding one note per conversation in
# $ANNOTATOR_STORE. A write naming an id keeps that id and rewrites the record;
# a write naming none mints one. Reads return whatever the store holds, so an
# edit or a move made against this annotator is visible on the next read.
# Invoked as `replacing.sh <store> <operation>`: claude-history appends the
# operation to whatever the registered command line holds, so the store travels
# in the registration rather than in the environment.
if [ "$#" -ge 2 ]; then
  store="$1"
  op="$2"
else
  store="${ANNOTATOR_STORE:-/tmp/replacing-annotator}"
  op="$1"
fi
payload=$(cat)
mkdir -p "$store"
export STORE="$store"
printf '%s' "$payload" | ANNOTATOR_OP="$op" python3 -c '
import json, os, pathlib, sys

store = pathlib.Path(os.environ["STORE"])
op = os.environ["ANNOTATOR_OP"]
request = json.load(sys.stdin)
path = store / "notes.json"
held = json.loads(path.read_text()) if path.exists() else {}

if op == "read":
    out = []
    for conversation in request["conversations"]:
        for record in held.get(conversation, []):
            out.append({"conversation": conversation, **record})
    print(json.dumps({"annotations": out}))
elif op == "write":
    conversation = request.pop("conversation")
    replaces = request.pop("replaces", None)
    rows = held.get(conversation, [])
    if replaces:
        request["id"] = replaces
        rows = [request if row.get("id") == replaces else row for row in rows]
    else:
        request["id"] = "replacing_%d" % (len(rows) + 1)
        rows = rows + [request]
    held[conversation] = rows
    path.write_text(json.dumps(held))
    print(json.dumps({"id": request["id"]}))
elif op == "delete":
    conversation = request["conversation"]
    rows = held.get(conversation, [])
    kept = [row for row in rows if row.get("id") != request["id"]]
    held[conversation] = kept
    path.write_text(json.dumps(held))
    print(json.dumps({"deleted": len(kept) != len(rows)}))
'
