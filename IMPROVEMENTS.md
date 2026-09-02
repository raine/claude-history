Semantic index improvements

Author: Richerland Medeiros <rick.land@gmail.com>
Date: 2026-08-22

I use claude-history every day to dig through my own Claude Code sessions,
and semantic search was the feature I kept reaching for and kept waiting on.
These changes make the in memory index incremental per conversation and the
ranking measurably faster, on top of the bounded retrieval work already on
main. Results are bit identical to the current implementation: every existing
test passes, including the ones that compare scores and explanations with
exact values, and the cache format is untouched.

What changed

1. Resident index per conversation

   Embeddings are resident per conversation instead of one flat list for the
   whole corpus. A refresh only re chunks the conversations that changed; the
   rest stay resident untouched. With a bounded refresh (max_new_embeddings),
   a conversation whose chunks are all present is complete and keeps its
   signature, while one left with uncached chunks stays rankable with what it
   has and is planned again on the next refresh, so partial coverage still
   ranks and later refreshes complete it. missing_chunk_count keeps the same
   meaning as before.

2. Ranking without copies

   Each query used to clone the text and the 384 float embedding of every
   chunk in scope before scoring. Ranking now borrows the resident chunks and
   derives the per conversation winners cloning only the winners.

3. Query independent text work done once

   Lowercasing, search normalization and the evidence preview were recomputed
   for every chunk on every keystroke. They are now computed once when a
   conversation becomes resident and reused. Query side normalization is done
   once per query instead of once per chunk. Scoring runs in parallel with
   rayon, order preserving, with a deterministic sort at the end.

4. Lighter signatures

   The corpus signature used to clone the semantic turns of every
   conversation, doubling the text in memory on each refresh. It now holds
   the Arc of the conversation and compares by pointer, falling back to a
   by reference comparison only when the corpus was reloaded.

5. Smaller files

   index.rs (1464 lines) became index.rs, index/signature.rs,
   index/resident.rs and index/tests/{mod,rank,refresh,corpus}.rs, none
   above 450 lines. Public API unchanged. cache.rs is not touched.

Numbers

   Synthetic, 20000 chunks of 1571 bytes, 384 dims, 3 word query, release,
   Apple Silicon, compared with the current main:

     rank per query        250 ms  ->   28 ms
     signature fast path   1.4 ms  -> 0.02 ms
     refresh from cache    118 ms  ->  140 ms (one time text preparation)

   The bench lives in src/semantic/index/tests/refresh.rs as an ignored
   test: cargo test --release bench_rank -- --ignored --nocapture

Trade off

   Two derived strings are kept per resident chunk (lowercase and
   normalized), about twice the chunk text in memory. The preview is only
   stored when it differs from the text.

Verification

   cargo test: 828 passed. cargo clippy: no new warnings. Real corpus smoke
   run on my own history: five searches ten seconds apart, same scores and
   byte identical output on the runs without new chunks.
