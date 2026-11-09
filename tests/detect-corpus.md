# GitHub project detection audit

Selected 2026-11-09; detector commit `244ab44ccf1a` (PR #20 baseline).

## Method

Five manually qualified web projects per supported language, ordered by GitHub stars among non-fork, non-archived repositories. Language means GitHub’s primary-language classification, not a measured ranking of the languages themselves. Production learning platforms qualify; tutorial collections, lists, coding agents, general runtimes, native desktop apps, and generic infrastructure tools do not. Eligibility is a judgment call, not an objective GitHub category.

Examples excluded above selected projects: Node.js (general runtime), Ollama (model runtime), Kubernetes (infrastructure), frp (generic networking), and developer-roadmap (educational roadmap collection). Stars and language ranks are frozen with full commit SHAs in [`detect-corpus.json`](detect-corpus.json).

Checks target reviewed project paths and static manifest declarations, not a matching framework somewhere in the repository’s examples. Identity checks separately ask whether framework implementations identify themselves. Catalog checks ask for the local catalog declaration rather than an unresolved reference. These last two categories probe capabilities beyond the current dependency-only behavior.

GitHub source archives were downloaded into a temporary cache. No dependencies were installed and no repository code was executed. Submodules and LFS targets were not fetched; symlinks were not extracted. The corpus occupies approximately 4.1 GiB. This is a sampled assertion suite, not an exhaustive precision/recall or buildability evaluation.

## Results

All **35/35** scans succeeded and produced identical output across five runs. **156/173 assertions passed**, and 23/35 repositories passed every sampled assertion.

| Category | Passed |
|---|---:|
| language | 35/35 |
| project | 35/35 |
| name | 26/27 |
| version | 32/32 |
| framework | 19/20 |
| workspace | 9/15 |
| identity | 0/7 |
| catalog | 0/2 |

“Version” checks cover selected literal constraints and inherited declarations, not installed versions. Framework checks cover selected consumer projects, while identity checks cover framework implementation packages.

### Gaps found

- **pnpm workspaces:** Next.js, freeCodeCamp, Vue, n8n, Dify, and Discourse lack Node workspace labels/member relationships. Their component projects are discovered individually. Cargo, npm/Yarn, and uv representatives passed.
- **Rails component dependencies:** Discourse declares `actionpack`, `railties`, and the other Rails components instead of `gem "rails"`; its root is reported as Bundler, not Rails.
- **Poetry names:** AutoGPT’s `autogpt_platform/backend/pyproject.toml` declares `tool.poetry.name = "autogpt-platform-backend"`, but the project’s names array is empty.
- **Framework identities:** React (`packages/react`), Next.js (`packages/next`), Vue, Laravel, Symfony, Rails, and Gin are not framework-labeled at their implementation package paths. Consumers/examples elsewhere must not mask this. A future identity rule should distinguish “provides” from “uses” rather than assume these packages are deployable applications.
- **pnpm catalogs:** n8n and Dify expose `catalog:typescript-tooling` / `catalog:` as TypeScript requirements without resolving the local `npm:@typescript/typescript6@6.0.2` declaration.

Additional unscored coverage gaps include Jekyll, Hugo, Dioxus, Ember, Gradio, and aiohttp. Their generic language/project fallback is not counted as a failed framework assertion. Test fixtures and example projects are intentionally scanned, so the report is not a list of deployable services.

### Timing

Each repository was scanned through the release CLI five times, serially. The table reports the median of runs 2–5 with a warm filesystem, including process startup, traversal, JSON serialization, and stdout transfer. Python JSON parsing and snapshot downloads are excluded. These are local measurements, not portable timing budgets or controlled cold-cache benchmarks.

Across the corpus, the scanner counted 259,818 files and read only 2,204 manifest/version files (1.77 MiB). The median repository time was 7.9 ms; the slowest was Next.js at 168.4 ms across 16,954 directories and 853 reported projects.

| Language | Repository | Stars | Language rank | Revision | Assertions | Median ms |
|---|---|---:|---:|---|---:|---:|
| JavaScript | [react/react](https://github.com/react/react) | 252,455 | 2 | [`b05c0b75fd97`](https://github.com/react/react/tree/b05c0b75fd9778536152eead30814c9ca90c810a) | 5/6 | 14.47 |
| JavaScript | [vercel/next.js](https://github.com/vercel/next.js) | 143,437 | 8 | [`5368e013e74e`](https://github.com/vercel/next.js/tree/5368e013e74e4e4ed5be51c051c989545c4450b9) | 5/7 | 168.41 |
| JavaScript | [mrdoob/three.js](https://github.com/mrdoob/three.js) | 117,953 | 11 | [`3f7edc3d75e0`](https://github.com/mrdoob/three.js/tree/3f7edc3d75e054bc755dad56c1c95c62f63a3561) | 3/3 | 4.31 |
| JavaScript | [axios/axios](https://github.com/axios/axios) | 109,295 | 14 | [`335e2f8464f5`](https://github.com/axios/axios/tree/335e2f8464f58a3ab485486b8d6964fea3e3f30a) | 4/4 | 2.18 |
| JavaScript | [react/create-react-app](https://github.com/react/create-react-app) | 103,230 | 15 | [`6254386531d2`](https://github.com/react/create-react-app/tree/6254386531d263688ccfa542d0e628fbc0de0b28) | 6/6 | 2.30 |
| TypeScript | [freeCodeCamp/freeCodeCamp](https://github.com/freeCodeCamp/freeCodeCamp) | 458,826 | 1 | [`11442d669447`](https://github.com/freeCodeCamp/freeCodeCamp/tree/11442d66944785ac72a2a2b9c955fae8b9f33083) | 5/6 | 14.84 |
| TypeScript | [vuejs/vue](https://github.com/vuejs/vue) | 212,624 | 7 | [`9e8870794008`](https://github.com/vuejs/vue/tree/9e88707940088cb1f4cd7dd210c9168a50dc347c) | 4/6 | 2.58 |
| TypeScript | [n8n-io/n8n](https://github.com/n8n-io/n8n) | 209,927 | 8 | [`93243edc00b2`](https://github.com/n8n-io/n8n/tree/93243edc00b288a840fbb562a5e9f64177f2337f) | 5/7 | 60.35 |
| TypeScript | [firecrawl/firecrawl](https://github.com/firecrawl/firecrawl) | 200,129 | 9 | [`40c8ff95d719`](https://github.com/firecrawl/firecrawl/tree/40c8ff95d719d2acbd8fd6273bd19b14a1c2bbb8) | 5/5 | 4.86 |
| TypeScript | [excalidraw/excalidraw](https://github.com/excalidraw/excalidraw) | 135,400 | 15 | [`701b901a0f1b`](https://github.com/excalidraw/excalidraw/tree/701b901a0f1ba3d0b390d9cdf63d112e25abac4b) | 7/7 | 3.94 |
| Python | [Significant-Gravitas/AutoGPT](https://github.com/Significant-Gravitas/AutoGPT) | 187,972 | 11 | [`9cb76f9026d3`](https://github.com/Significant-Gravitas/AutoGPT/tree/9cb76f9026d388c9bf2e098af2beae1cc864f055) | 5/6 | 14.63 |
| Python | [AUTOMATIC1111/stable-diffusion-webui](https://github.com/AUTOMATIC1111/stable-diffusion-webui) | 165,269 | 14 | [`82a973c04367`](https://github.com/AUTOMATIC1111/stable-diffusion-webui/tree/82a973c04367123ae98bd9abdf80d9eda9b910e2) | 3/3 | 1.60 |
| Python | [langgenius/dify](https://github.com/langgenius/dify) | 160,774 | 15 | [`fdb09a9e76dc`](https://github.com/langgenius/dify/tree/fdb09a9e76dc8cbd052fabd72cfff4071328d9bc) | 8/10 | 30.46 |
| Python | [langflow-ai/langflow](https://github.com/langflow-ai/langflow) | 156,479 | 16 | [`60999cc9a735`](https://github.com/langflow-ai/langflow/tree/60999cc9a7350e44dde3ef641baae864e10861f0) | 6/6 | 20.32 |
| Python | [Comfy-Org/ComfyUI](https://github.com/Comfy-Org/ComfyUI) | 145,983 | 20 | [`540a82ee722b`](https://github.com/Comfy-Org/ComfyUI/tree/540a82ee722bda1f32a1fbd97fcadadeb22d548b) | 4/4 | 3.49 |
| PHP | [coollabsio/coolify](https://github.com/coollabsio/coolify) | 65,849 | 2 | [`30335d80866f`](https://github.com/coollabsio/coolify/tree/30335d80866ff3391d899cf4ccb599b696f47b7e) | 5/5 | 5.30 |
| PHP | [nextcloud/server](https://github.com/nextcloud/server) | 37,456 | 3 | [`d655757fb306`](https://github.com/nextcloud/server/tree/d655757fb306aca893651958030b5dae69ed0f39) | 3/3 | 14.62 |
| PHP | [laravel/framework](https://github.com/laravel/framework) | 34,995 | 4 | [`338e44065c0a`](https://github.com/laravel/framework/tree/338e44065c0a5b18f1f290605b6253a1e69d8f56) | 4/5 | 6.33 |
| PHP | [filamentphp/filament](https://github.com/filamentphp/filament) | 32,462 | 5 | [`9d78093cfebd`](https://github.com/filamentphp/filament/tree/9d78093cfebdc96c077b1d8dbf94b41738f28992) | 4/4 | 16.63 |
| PHP | [symfony/symfony](https://github.com/symfony/symfony) | 31,523 | 6 | [`f07928484c9e`](https://github.com/symfony/symfony/tree/f07928484c9e96646964deb7b9570d7885492512) | 4/5 | 10.10 |
| Ruby | [rails/rails](https://github.com/rails/rails) | 59,087 | 1 | [`3a0aff317bc0`](https://github.com/rails/rails/tree/3a0aff317bc037f43f168e3772028f1033f1e810) | 2/3 | 7.90 |
| Ruby | [jekyll/jekyll](https://github.com/jekyll/jekyll) | 51,756 | 2 | [`97c3bdc822df`](https://github.com/jekyll/jekyll/tree/97c3bdc822df9d5bea4c269b29a28de5a5e29d70) | 2/2 | 2.42 |
| Ruby | [mastodon/mastodon](https://github.com/mastodon/mastodon) | 50,502 | 3 | [`5a207bc8f504`](https://github.com/mastodon/mastodon/tree/5a207bc8f5046e27cffc640a3c64f96504f6afcf) | 4/4 | 8.68 |
| Ruby | [huginn/huginn](https://github.com/huginn/huginn) | 50,454 | 4 | [`639c050b2bf4`](https://github.com/huginn/huginn/tree/639c050b2bf4e14ef9983012af99dfc1dc68b334) | 4/4 | 2.35 |
| Ruby | [discourse/discourse](https://github.com/discourse/discourse) | 48,066 | 6 | [`899c68d783e7`](https://github.com/discourse/discourse/tree/899c68d783e7808c50144752369a685220250d7c) | 3/5 | 35.98 |
| Go | [infiniflow/ragflow](https://github.com/infiniflow/ragflow) | 93,887 | 8 | [`ed5a84fee9ab`](https://github.com/infiniflow/ragflow/tree/ed5a84fee9abc8091aea111d1195cf7768541ff1) | 5/5 | 10.20 |
| Go | [gohugoio/hugo](https://github.com/gohugoio/hugo) | 90,425 | 9 | [`d2010fcc9cd3`](https://github.com/gohugoio/hugo/tree/d2010fcc9cd30c5b4e9274ded4485f4222326b53) | 4/4 | 4.85 |
| Go | [gin-gonic/gin](https://github.com/gin-gonic/gin) | 89,609 | 11 | [`0f09c3a9b462`](https://github.com/gin-gonic/gin/tree/0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646) | 4/5 | 2.11 |
| Go | [netdata/netdata](https://github.com/netdata/netdata) | 81,330 | 14 | [`0d54b4424ea6`](https://github.com/netdata/netdata/tree/0d54b4424ea6f40c344738b9d566e1a26d160da3) | 4/4 | 23.42 |
| Go | [caddyserver/caddy](https://github.com/caddyserver/caddy) | 78,927 | 15 | [`9ea3108377d2`](https://github.com/caddyserver/caddy/tree/9ea3108377d2abec102866808133c40ac9582162) | 4/4 | 2.56 |
| Rust | [dani-garcia/vaultwarden](https://github.com/dani-garcia/vaultwarden) | 70,490 | 17 | [`17b159beff4b`](https://github.com/dani-garcia/vaultwarden/tree/17b159beff4b892f07f61f38ce2fe452e55251c1) | 6/6 | 2.51 |
| Rust | [meilisearch/meilisearch](https://github.com/meilisearch/meilisearch) | 59,822 | 29 | [`8c7bfcfa9bcf`](https://github.com/meilisearch/meilisearch/tree/8c7bfcfa9bcff7083bcb5d184fa2a523a23884d8) | 5/5 | 4.99 |
| Rust | [juspay/hyperswitch](https://github.com/juspay/hyperswitch) | 45,760 | 39 | [`5786b738c088`](https://github.com/juspay/hyperswitch/tree/5786b738c088b8452296ca19922fcdc0dc74e129) | 6/6 | 12.07 |
| Rust | [parcel-bundler/parcel](https://github.com/parcel-bundler/parcel) | 44,012 | 42 | [`20c0f2ede4c1`](https://github.com/parcel-bundler/parcel/tree/20c0f2ede4c1044092d44288169cce589c5d5c14) | 3/3 | 18.92 |
| Rust | [DioxusLabs/dioxus](https://github.com/DioxusLabs/dioxus) | 39,705 | 52 | [`17d9a5924e84`](https://github.com/DioxusLabs/dioxus/tree/17d9a5924e845e4f37deca3fada6c35d1d836af0) | 5/5 | 8.71 |

## Reproduce

```sh
cargo build --release --locked
cache=$(mktemp -d)
python3 tests/audit_detect.py --fetch --cache "$cache" --output "$cache/results.json"
```

Downloads require the explicit `--fetch` flag; omit it to rerun against an existing pinned cache entirely offline. Downloads are capped at 1 GiB compressed / 2 GiB expanded per repository. Existing unverified cache entries are not overwritten. The runner exits nonzero for missing assertions, command failures, or nondeterministic output; **exit 1 is expected for this baseline**.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
```

The runner uses Python 3.11+ and the standard library only. Full per-check results and repeat timings are written to the specified JSON output; downloaded repositories and generated reports are not committed.
