#!/bin/sh
# An annotator that refuses every operation, for the case where a store is
# unreachable and the transcript still has to render.
cat > /dev/null
exit 3
