# S6 — local part search + Rebrickable mapping

Question: can a fully local search find the part you mean in < 0.5 s, from a name, a Rebrickable name,
a typo or a plain description, using a small encoder (< 100 MB)? Which encoder?

## Catalog (`search-spike catalog`, 2.2 s)

24,735 LDraw parts joined with Rebrickable (`data/rebrickable/*.csv.gz`, 17 MB):

| Rebrickable inventory rows mapped by | share |
|---|---|
| exact part number | 84.4% |
| LDraw `~Moved to` | 4.0% |
| printed part → base shape | 3.9% |
| mould letter stripped (`3001a` → `3001`) | 0.6% |
| Rebrickable mould/alternate relationship | 0.5% |
| **unmapped** | **6.5%** |

**93.5%** of real-set part usage maps to an LDraw part. 5,233 parts get a popularity count and 7,053
get a Rebrickable name as a second name field. 204 of 275 Rebrickable colours map to LDraw codes;
the 71 that don't are HO, Duplo and vintage. That's enough for the "only parts that exist in this
colour" filter.

## Encoders (`golden.tsv`: 168 queries in 6 groups)

Best weights for each, found by sweeping `--w-emb` and `--w-pop`:

| | top-1 | top-5 | MRR | query p50 | RAM (whole searcher) | model on disk | index all parts |
|---|---|---|---|---|---|---|---|
| BM25F + priors only | 84.5% | 94.0% | 0.886 | **0.22 ms** | **98 MB** | — | — |
| + potion-base-8m (model2vec, 256-d) | **86.3%** | **94.6%** | **0.903** | 6.1 ms | 180 MB | 30 MB | **0.5 s** |
| + all-MiniLM-L6-v2 (BERT, 384-d, candle) | 86.3% | 94.0% | 0.897 | 15.7 ms | 264 MB | 87 MB | 226 s |

- **potion-base-8m wins.** It matches MiniLM's quality, with 2.5× lower query latency, ~85 MB less
  RAM and ~450× faster indexing. It is a static lookup table, so no transformer runs at query time.
- **Encoders add little on their own.** At the default weight (0.8), either one *lowered* top-1 to
  73–75% because general sentence models don't know LEGO jargon. They only help as a light
  tie-breaker (weight 0.1–0.2), adding +2 describe queries (26 → 28 of 38).
- **Popularity is the biggest lever.** Raising the Rebrickable popularity weight from 0.6 to 1.0
  moved top-1 from 81.5% to 84.5% with no encoder at all.
- The 6 ms potion "embed" time is almost all the brute-force scan over 24,735 × 256 f32 vectors plus
  the tokenizers crate's batch overhead. Scoring only the ~300 BM25 candidates, plus a small
  f16/SIMD scan for recall, should get it under 1 ms.
- Everything is far inside the 0.5 s goal. p95 is 8 ms with the encoder and 2 ms without.

Defaults in `src/search.rs` are now the tuned set (emb 0.2, pop 1.5).

## Remaining misses (20 of 168; see `eval --fails`)

- **Variant ties (rank 2):** "1x1 cone" → *Cone with Stop*, "technic pin" → *½ pin*, "tile 1x1
  with clip" → thick-clip variant, "plate with handle on side" → *Handle* vs *Handle Type 2*. Fix:
  a per-family "canonical member" prior (most popular in its mould group), plus a penalty for extra
  qualifier words the query didn't ask for.
- **Dimension parsing over-boosts plain bricks and plates:** "liftarm 1x3" returns *Plate 1x3*
  because liftarms are named by length ("Beam 3"). Fix: map NxM to the length for technic
  families, and stop the dims bonus from overriding a matched family word.
- **Printed-only parts score too low:** minifig torso, legs, body. Nearly all real usage is printed,
  and the print → base mapping misses assemblies (`3815c01`). Fix: roll print popularity up into
  `c01` assemblies, plus synonyms (`legs` → *hips and legs*).
- **Description gaps:** "small flat square piece with no studs", "stick 4 studs long", "pin to
  connect technic beams". These are what a LEGO-specific encoder would fix. Option for M5:
  distil or fine-tune potion on (Rebrickable name, LDraw name, keywords, generated descriptions)
  pairs. model2vec training takes minutes, so it can be iterated on this golden set.

M5 targets: top-1 ≥ 90%, MRR ≥ 0.9 (✓), p95 ≤ 30 ms (✓), embedding RAM < 100 MB (✓: 30 MB model +
24 MB corpus). The fixes above should cover most of the 3.7 pp to 90%.

## Files

- `src/catalog.rs`: LDraw + Rebrickable join, colour map, popularity.
- `src/text.rs`: analyzer (dims normalisation, number words, stems, typo distance).
- `src/search.rs`: BM25F, synonyms, colour extraction, embedding fusion, priors.
- `src/embed.rs`: potion (model2vec) and BERT encoders.
- `golden.tsv`: evaluation queries.
- `out/`: `catalog.json` and `emb-*.bin` (regenerate with `catalog` / `index --enc NAME`).
