# Foundation assessment

Research date: 2026-06-20

## Verdict

Keep `search.md`. The thesis is right: French legal search for LLM agents should be structure-aware, temporal, hybrid, authority-weighted, citation-grounded, and designed for multi-step retrieval.

The main improvement is to make the hard implementation constraints explicit. The document currently reads like architectural principles; the next version should name the failure modes that matter for French legal data.

## Key points

### Temporal search is the core differentiator

LEGI explicitly keeps modified and abrogated versions alongside texts in force. That means `valid_from`, `valid_to`, status, version grouping, and `as_of` filtering should be non-negotiable, not an optional enhancement.

Practical implication: every statutory hit should answer "which version, valid when, from which official source?"

### Decision chunking must be zone-aware

Judilibre exposes structured zones such as introduction, moyens, motivations, dispositif, moyens annexes, summary, visa, and rapprochements. But zone fragments can be non-sequential in the full text.

Practical implication: decision chunking should use official offsets/zones when present. Regex-only splitting will be brittle and can mix legal reasoning with procedural arguments or annexes.

### Hybrid retrieval is the right default

French legal queries combine exact references and conceptual phrasing:

- Exact examples: `Article 1240 du Code civil`, `L. 217-4`, pourvoi numbers, ECLI.
- Conceptual examples: `rupture de contrat injustifiee`, `vice cache`, `licenciement abusif`.

BM25/sparse retrieval is needed for exact statutory and citation syntax. Dense retrieval is needed for paraphrase and lay-to-legal vocabulary. A multilingual model such as BGE-M3 is plausible because it supports dense, sparse, multi-vector, multilingual, and long-input retrieval, but it should be benchmarked rather than assumed.

### Authority weighting is supported by real metadata

Judilibre metadata includes jurisdiction, chamber, formation, pourvoi number, ECLI, publication level, solution, decision date, full text, zones, visa, and jurisprudence rapprochements.

Practical implication: authority filtering and ranking are feasible. They should remain ranking signals, not hard filters, because local court decisions can be useful for quantum, trends, and factual analogies.

### Citation verification is mandatory

Legal LLM hallucination is empirically documented, and ECLI exists specifically to identify, cite, search, and link judicial decisions. A French legal search tool for agents should provide a strict citation-resolution path, not just retrieve snippets.

Practical implication: every returned result should carry stable identifiers and an official URL. Generated citations should be re-resolvable through a `cite`/verification operation before use.

### Graph-RAG should be modest and source-backed

A graph layer is useful for:

- `decision -> cites/applies -> article`
- `article -> interpreted_by -> decision`
- `decision -> rapprochements -> decision`
- `article version -> supersedes/superseded_by -> article version`

But the graph should not pretend to automatically prove `jurisprudence constante`. That requires authority ranking, publication metadata, recency, citation frequency, and human-checkable sources.

## Suggested addition to `search.md`

Add a short section named "Non-negotiable implementation constraints":

- Use official-source provenance for every indexed object.
- Preserve versioned statutory IDs and validity windows.
- Record corpus freshness and update provenance.
- Use official decision zones/offsets before heuristic chunking.
- Keep search and fetch separate to control agent token usage.
- Provide strict citation verification over IDs, ECLI, pourvoi, NOR, and Légifrance identifiers.
- Evaluate retrieval with legal tasks, not only generic semantic similarity.

## Sources

- Légifrance open data and API: https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- LEGI dataset: https://www.data.gouv.fr/datasets/legi-codes-lois-et-reglements-consolides
- Judilibre dataset/API metadata: https://www.data.gouv.fr/datasets/api-judilibre
- Judilibre zone-fragment behavior: https://github.com/Cour-de-cassation/judilibre-search
- Justice administrative open data: https://www.conseil-etat.fr/decisions-de-justice/donnees-ouvertes-open-data
- ECLI identifier: https://eur-lex.europa.eu/content/help/eurlex-content/ecli.html
- BGE-M3 model: https://huggingface.co/BAAI/bge-m3
- BGE-M3 paper: https://arxiv.org/html/2402.03216v3
- Legal hallucination study: https://academic.oup.com/jla/article/16/1/64/7699227
