# GitHub project detection audit

Selected 2026-11-09. Baseline: detector commit `244ab44ccf1a` (PR #20). Current: the follow-up fixes in this revision.

## Method

Five manually qualified web projects per supported language, ordered by GitHub stars among non-fork, non-archived repositories. Language means GitHub’s primary-language classification, not a measured ranking of the languages themselves. Production learning platforms qualify; tutorial collections, lists, coding agents, general runtimes, native desktop apps, and generic infrastructure tools do not. Eligibility is a judgment call, not an objective GitHub category.

Examples excluded above selected projects: Node.js (general runtime), Ollama (model runtime), Kubernetes (infrastructure), frp (generic networking), and developer-roadmap (educational roadmap collection). Stars and language ranks are frozen with full commit SHAs in [`detect-corpus.json`](detect-corpus.json).

Checks target reviewed project paths and static manifest declarations, not a matching framework somewhere in the repository’s examples. Identity checks separately ask whether framework implementations identify themselves. Catalog checks ask for the local catalog declaration rather than an unresolved reference. These last two categories exposed capabilities missing from the baseline dependency-only behavior.

GitHub source archives were downloaded into a temporary cache. No application dependencies were installed and no downloaded repository code was executed. Submodules and LFS targets were not fetched; symlinks were not extracted. The corpus occupies approximately 4.1 GiB. This is a sampled assertion suite, not an exhaustive precision/recall or buildability evaluation.

## Results

All **35/35** scans succeed and produce identical output across five runs. **173/173 sampled assertions now pass**, up from 156/173. Each of the 35 ranked repositories passes its sampled assertions; this is not a claim of exhaustive detection accuracy.

| Category | Baseline | Current |
|---|---:|---:|
| language | 35/35 | 35/35 |
| project | 35/35 | 35/35 |
| name | 26/27 | 27/27 |
| version | 32/32 | 32/32 |
| framework | 19/20 | 20/20 |
| workspace | 9/15 | 15/15 |
| identity | 0/7 | 7/7 |
| catalog | 0/2 | 2/2 |

“Version” checks cover selected literal constraints and inherited declarations, not installed versions. Framework checks cover selected consumer projects, while identity checks cover framework implementation packages.

### Misses fixed

- **pnpm workspaces:** Next.js, freeCodeCamp, Vue, n8n, Dify, and Discourse now report their Node workspace labels and members, including negative patterns. A separate directory-glob cross-check found all 225 package members across these six snapshots, with no extra members. Cargo, npm/Yarn, and uv representatives still pass.
- **Rails components:** Discourse is recognized from the conjunction of `actionpack` and `railties`. `activesupport` or either web component alone does not label a project as Rails.
- **Poetry names:** AutoGPT’s backend retains its declared `autogpt-platform-backend` name. Names preserve declared spelling; dependency identity matching uses ecosystem normalization.
- **Framework identities:** React, Next.js, Vue, Laravel, Symfony, Rails, and Gin now identify their implementation packages. Framework detections distinguish `provides` from `uses`, rather than implying that a framework library is a deployable application. Ruby gemspec metadata is read statically, never evaluated.
- **pnpm catalogs:** Default and named catalogs resolve local version declarations while preserving both the reference and its YAML provenance. Missing entries, excluded packages, and malformed nested workspace files do not silently inherit unrelated catalog values.

Additional unscored coverage gaps include Jekyll, Hugo, Dioxus, Ember, Gradio, and aiohttp. Their generic language/project fallback is not counted as a failed framework assertion. Test fixtures and example projects are intentionally scanned, so the report is not a list of deployable services.

### Timing

Each repository was scanned through the release CLI five times, serially. The table reports the median of runs 2–5 with a warm filesystem, including process startup, traversal, JSON serialization, and stdout transfer. Python JSON parsing and snapshot downloads are excluded. These are local measurements, not portable timing budgets or controlled cold-cache benchmarks.

Across the corpus, the scanner counted 259,818 files and read only 2,270 manifest/version files (1.85 MiB). The current median repository time is 5.4 ms.

An interleaved, same-machine A/B comparison ran the baseline and current binaries 12 times each, discarding the first two pairs: **Next.js improved from 170.8 ms to 86.8 ms** (1.97× faster), with the same full-tree scope. The generated 200-service / 20,200-file benchmark improved from 5.8 ms to **3.5 ms median**, still reading only 200 manifests.

Profiling identified directory traversal and repeated path/ancestor lookups as the main costs. Changes index document lookups, retain relevant directory facts only, use parent indices for source ownership/version inheritance, and prefilter workspace candidates by manifest type. Wide directory trees use at most four workers; small frontiers remain serial. Buffered JSON output avoids per-line stdout flushes. Before adding the detection fixes, the indexing/ownership optimizations alone produced byte-identical results on all 35 repositories.

pnpm uses `serde-saphyr` with deserialization only: typed fields are populated directly without constructing a syntax tree, retaining comments, enabling includes, or evaluating application code. A tree-sitter syntax tree is unnecessary for this manifest-only scan.

| Language | Repository | Stars | Language rank | Revision | Baseline assertions | Current assertions | Before ms | After ms |
|---|---|---:|---:|---|---:|---:|---:|---:|
| JavaScript | [react/react](https://github.com/react/react) | 252,455 | 2 | [`b05c0b75fd97`](https://github.com/react/react/tree/b05c0b75fd9778536152eead30814c9ca90c810a) | 5/6 | 6/6 | 14.47 | 7.29 |
| JavaScript | [vercel/next.js](https://github.com/vercel/next.js) | 143,437 | 8 | [`5368e013e74e`](https://github.com/vercel/next.js/tree/5368e013e74e4e4ed5be51c051c989545c4450b9) | 5/7 | 7/7 | 168.41 | 85.82 |
| JavaScript | [mrdoob/three.js](https://github.com/mrdoob/three.js) | 117,953 | 11 | [`3f7edc3d75e0`](https://github.com/mrdoob/three.js/tree/3f7edc3d75e054bc755dad56c1c95c62f63a3561) | 3/3 | 3/3 | 4.31 | 3.74 |
| JavaScript | [axios/axios](https://github.com/axios/axios) | 109,295 | 14 | [`335e2f8464f5`](https://github.com/axios/axios/tree/335e2f8464f58a3ab485486b8d6964fea3e3f30a) | 4/4 | 4/4 | 2.18 | 2.61 |
| JavaScript | [react/create-react-app](https://github.com/react/create-react-app) | 103,230 | 15 | [`6254386531d2`](https://github.com/react/create-react-app/tree/6254386531d263688ccfa542d0e628fbc0de0b28) | 6/6 | 6/6 | 2.30 | 2.26 |
| TypeScript | [freeCodeCamp/freeCodeCamp](https://github.com/freeCodeCamp/freeCodeCamp) | 458,826 | 1 | [`11442d669447`](https://github.com/freeCodeCamp/freeCodeCamp/tree/11442d66944785ac72a2a2b9c955fae8b9f33083) | 5/6 | 6/6 | 14.84 | 10.73 |
| TypeScript | [vuejs/vue](https://github.com/vuejs/vue) | 212,624 | 7 | [`9e8870794008`](https://github.com/vuejs/vue/tree/9e88707940088cb1f4cd7dd210c9168a50dc347c) | 4/6 | 6/6 | 2.58 | 2.70 |
| TypeScript | [n8n-io/n8n](https://github.com/n8n-io/n8n) | 209,927 | 8 | [`93243edc00b2`](https://github.com/n8n-io/n8n/tree/93243edc00b288a840fbb562a5e9f64177f2337f) | 5/7 | 7/7 | 60.35 | 32.57 |
| TypeScript | [firecrawl/firecrawl](https://github.com/firecrawl/firecrawl) | 200,129 | 9 | [`40c8ff95d719`](https://github.com/firecrawl/firecrawl/tree/40c8ff95d719d2acbd8fd6273bd19b14a1c2bbb8) | 5/5 | 5/5 | 4.86 | 4.54 |
| TypeScript | [excalidraw/excalidraw](https://github.com/excalidraw/excalidraw) | 135,400 | 15 | [`701b901a0f1b`](https://github.com/excalidraw/excalidraw/tree/701b901a0f1ba3d0b390d9cdf63d112e25abac4b) | 7/7 | 7/7 | 3.94 | 2.66 |
| Python | [Significant-Gravitas/AutoGPT](https://github.com/Significant-Gravitas/AutoGPT) | 187,972 | 11 | [`9cb76f9026d3`](https://github.com/Significant-Gravitas/AutoGPT/tree/9cb76f9026d388c9bf2e098af2beae1cc864f055) | 5/6 | 6/6 | 14.63 | 11.53 |
| Python | [AUTOMATIC1111/stable-diffusion-webui](https://github.com/AUTOMATIC1111/stable-diffusion-webui) | 165,269 | 14 | [`82a973c04367`](https://github.com/AUTOMATIC1111/stable-diffusion-webui/tree/82a973c04367123ae98bd9abdf80d9eda9b910e2) | 3/3 | 3/3 | 1.60 | 1.93 |
| Python | [langgenius/dify](https://github.com/langgenius/dify) | 160,774 | 15 | [`fdb09a9e76dc`](https://github.com/langgenius/dify/tree/fdb09a9e76dc8cbd052fabd72cfff4071328d9bc) | 8/10 | 10/10 | 30.46 | 18.70 |
| Python | [langflow-ai/langflow](https://github.com/langflow-ai/langflow) | 156,479 | 16 | [`60999cc9a735`](https://github.com/langflow-ai/langflow/tree/60999cc9a7350e44dde3ef641baae864e10861f0) | 6/6 | 6/6 | 20.32 | 12.02 |
| Python | [Comfy-Org/ComfyUI](https://github.com/Comfy-Org/ComfyUI) | 145,983 | 20 | [`540a82ee722b`](https://github.com/Comfy-Org/ComfyUI/tree/540a82ee722bda1f32a1fbd97fcadadeb22d548b) | 4/4 | 4/4 | 3.49 | 3.34 |
| PHP | [coollabsio/coolify](https://github.com/coollabsio/coolify) | 65,849 | 2 | [`30335d80866f`](https://github.com/coollabsio/coolify/tree/30335d80866ff3391d899cf4ccb599b696f47b7e) | 5/5 | 5/5 | 5.30 | 4.41 |
| PHP | [nextcloud/server](https://github.com/nextcloud/server) | 37,456 | 3 | [`d655757fb306`](https://github.com/nextcloud/server/tree/d655757fb306aca893651958030b5dae69ed0f39) | 3/3 | 3/3 | 14.62 | 7.52 |
| PHP | [laravel/framework](https://github.com/laravel/framework) | 34,995 | 4 | [`338e44065c0a`](https://github.com/laravel/framework/tree/338e44065c0a5b18f1f290605b6253a1e69d8f56) | 4/5 | 5/5 | 6.33 | 5.06 |
| PHP | [filamentphp/filament](https://github.com/filamentphp/filament) | 32,462 | 5 | [`9d78093cfebd`](https://github.com/filamentphp/filament/tree/9d78093cfebdc96c077b1d8dbf94b41738f28992) | 4/4 | 4/4 | 16.63 | 15.30 |
| PHP | [symfony/symfony](https://github.com/symfony/symfony) | 31,523 | 6 | [`f07928484c9e`](https://github.com/symfony/symfony/tree/f07928484c9e96646964deb7b9570d7885492512) | 4/5 | 5/5 | 10.10 | 9.20 |
| Ruby | [rails/rails](https://github.com/rails/rails) | 59,087 | 1 | [`3a0aff317bc0`](https://github.com/rails/rails/tree/3a0aff317bc037f43f168e3772028f1033f1e810) | 2/3 | 3/3 | 7.90 | 5.62 |
| Ruby | [jekyll/jekyll](https://github.com/jekyll/jekyll) | 51,756 | 2 | [`97c3bdc822df`](https://github.com/jekyll/jekyll/tree/97c3bdc822df9d5bea4c269b29a28de5a5e29d70) | 2/2 | 2/2 | 2.42 | 2.12 |
| Ruby | [mastodon/mastodon](https://github.com/mastodon/mastodon) | 50,502 | 3 | [`5a207bc8f504`](https://github.com/mastodon/mastodon/tree/5a207bc8f5046e27cffc640a3c64f96504f6afcf) | 4/4 | 4/4 | 8.68 | 7.09 |
| Ruby | [huginn/huginn](https://github.com/huginn/huginn) | 50,454 | 4 | [`639c050b2bf4`](https://github.com/huginn/huginn/tree/639c050b2bf4e14ef9983012af99dfc1dc68b334) | 4/4 | 4/4 | 2.35 | 2.06 |
| Ruby | [discourse/discourse](https://github.com/discourse/discourse) | 48,066 | 6 | [`899c68d783e7`](https://github.com/discourse/discourse/tree/899c68d783e7808c50144752369a685220250d7c) | 3/5 | 5/5 | 35.98 | 21.07 |
| Go | [infiniflow/ragflow](https://github.com/infiniflow/ragflow) | 93,887 | 8 | [`ed5a84fee9ab`](https://github.com/infiniflow/ragflow/tree/ed5a84fee9abc8091aea111d1195cf7768541ff1) | 5/5 | 5/5 | 10.20 | 5.38 |
| Go | [gohugoio/hugo](https://github.com/gohugoio/hugo) | 90,425 | 9 | [`d2010fcc9cd3`](https://github.com/gohugoio/hugo/tree/d2010fcc9cd30c5b4e9274ded4485f4222326b53) | 4/4 | 4/4 | 4.85 | 3.15 |
| Go | [gin-gonic/gin](https://github.com/gin-gonic/gin) | 89,609 | 11 | [`0f09c3a9b462`](https://github.com/gin-gonic/gin/tree/0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646) | 4/5 | 5/5 | 2.11 | 1.70 |
| Go | [netdata/netdata](https://github.com/netdata/netdata) | 81,330 | 14 | [`0d54b4424ea6`](https://github.com/netdata/netdata/tree/0d54b4424ea6f40c344738b9d566e1a26d160da3) | 4/4 | 4/4 | 23.42 | 11.05 |
| Go | [caddyserver/caddy](https://github.com/caddyserver/caddy) | 78,927 | 15 | [`9ea3108377d2`](https://github.com/caddyserver/caddy/tree/9ea3108377d2abec102866808133c40ac9582162) | 4/4 | 4/4 | 2.56 | 2.33 |
| Rust | [dani-garcia/vaultwarden](https://github.com/dani-garcia/vaultwarden) | 70,490 | 17 | [`17b159beff4b`](https://github.com/dani-garcia/vaultwarden/tree/17b159beff4b892f07f61f38ce2fe452e55251c1) | 6/6 | 6/6 | 2.51 | 2.76 |
| Rust | [meilisearch/meilisearch](https://github.com/meilisearch/meilisearch) | 59,822 | 29 | [`8c7bfcfa9bcf`](https://github.com/meilisearch/meilisearch/tree/8c7bfcfa9bcff7083bcb5d184fa2a523a23884d8) | 5/5 | 5/5 | 4.99 | 5.17 |
| Rust | [juspay/hyperswitch](https://github.com/juspay/hyperswitch) | 45,760 | 39 | [`5786b738c088`](https://github.com/juspay/hyperswitch/tree/5786b738c088b8452296ca19922fcdc0dc74e129) | 6/6 | 6/6 | 12.07 | 7.95 |
| Rust | [parcel-bundler/parcel](https://github.com/parcel-bundler/parcel) | 44,012 | 42 | [`20c0f2ede4c1`](https://github.com/parcel-bundler/parcel/tree/20c0f2ede4c1044092d44288169cce589c5d5c14) | 3/3 | 3/3 | 18.92 | 11.77 |
| Rust | [DioxusLabs/dioxus](https://github.com/DioxusLabs/dioxus) | 39,705 | 52 | [`17d9a5924e84`](https://github.com/DioxusLabs/dioxus/tree/17d9a5924e845e4f37deca3fada6c35d1d836af0) | 5/5 | 5/5 | 8.71 | 7.17 |

## Additional validation: Brainpod starters

Ran the release detector against all ten `case/*` branches of [`brainpodnl/starters`](https://github.com/brainpodnl/starters), pinning each commit below. The default branch is `case/bookstack`. This is a separate starter smoke test, not an expansion of the 173 ranked-corpus assertions above.

Each branch was scanned five times without installing application dependencies or executing downloaded code. All ten scans succeeded and repeated byte-identically. Seven expected main stacks were recognized; Outline is partial because Koa has no rule, and eShop / Spring Petclinic are outside the supported language/manifest set. Auxiliary docs/assets must not be counted as detection of eShop’s main application.

Timings are warm-filesystem, end-to-end CLI medians of runs 2–5. Empty or partial output is not treated as an accuracy pass merely because it is fast.

| Branch | Revision | Result | Median ms |
|---|---|---|---:|
| `case/bookstack` | [`6265d5667dd6`](https://github.com/brainpodnl/starters/tree/6265d5667dd6904b7aa0d05a8c60795cf104011b) | Laravel, Composer, JavaScript/TypeScript. PHP `^8.2.0` and Laravel `^v12.26.4` declarations retained. | 4.07 |
| `case/eshop` | [`f21d6233b851`](https://github.com/brainpodnl/starters/tree/f21d6233b851897371c25c5e7ffa67bb8552dba8) | **Unsupported primary stack:** .NET 10/C# not detected. Only the docs Gemfile and JavaScript assets appear. | 2.54 |
| `case/fastapi-fullstack` | [`84c5a9e11a9e`](https://github.com/brainpodnl/starters/tree/84c5a9e11a9e58262599e3d772f5386924e02ca7) | FastAPI backend, React/Vite frontend, npm and uv workspaces. Python `3.14` pin plus `>=3.14,<4.0` constraint retained; all three workspace members found. | 2.44 |
| `case/linkding` | [`24b5ad6cc9bd`](https://github.com/brainpodnl/starters/tree/24b5ad6cc9bde497b5d1b1e86aed5a2fb7d25c2f) | Django backend (`python >=3.13`) and Astro documentation project detected. | 2.52 |
| `case/maybe` | [`224f21354a91`](https://github.com/brainpodnl/starters/tree/224f21354a916dfbec29e9435db1045b3d61a94b) | Rails and Ruby `3.4.4` pin detected. Non-fatal `dynamic ruby declaration not evaluated` warning for `ruby file: ".ruby-version"`. | 3.09 |
| `case/miniflux` | [`faecfa2fac61`](https://github.com/brainpodnl/starters/tree/faecfa2fac61a91f4f69550bb1afd506c4b51749) | Go module `miniflux.app/v2`, Go `1.26.0` requirement detected; auxiliary scripts/assets are separate source projects. | 2.47 |
| `case/outline` | [`09f11188b87b`](https://github.com/brainpodnl/starters/tree/09f11188b87b0220fa318dfa6fdd0fa9772195dd) | **Partial:** Node/React/Vite, TypeScript, Node engine constraint and `.nvmrc` pin detected. Declared Koa `^3.0.3` backend is not labeled. | 4.06 |
| `case/saas-starter` | [`6e33e58b1e55`](https://github.com/brainpodnl/starters/tree/6e33e58b1e553a41fe22e6b941a7229a002de361) | Next.js/React and TypeScript detected. No package name or Node version is invented when absent from supported manifest fields. | 2.05 |
| `case/spring-petclinic` | [`500158f73241`](https://github.com/brainpodnl/starters/tree/500158f732419217507c7656904b8e6aa1bcc0d6) | **Unsupported primary stack:** Java 17 / Spring Boot `4.1.0` declared in Maven, but the detector returns no projects. | 2.16 |
| `case/uptime-kuma` | [`df601c58d920`](https://github.com/brainpodnl/starters/tree/df601c58d9205562f34cfe5ef35264b341019d19) | Express/Vue/Vite and Node `>=20.4.0` constraint detected, alongside auxiliary example projects. | 2.60 |

Koa needs an additional npm framework rule. C#/.NET and Java/Spring require new language and manifest support (`global.json` / MSBuild and Maven, respectively). The Maybe warning is redundant with the already detected Ruby pin. None of these observations was silently converted into a passing assertion or hidden by skipping a branch. The detector was unchanged during this validation.

## Reproduce

```sh
cargo build --release --locked
cache=$(mktemp -d)
python3 tests/audit_detect.py --fetch --cache "$cache" --output "$cache/results.json"
```

Downloads require the explicit `--fetch` flag; omit it to rerun against an existing pinned cache entirely offline. Downloads are capped at 1 GiB compressed / 2 GiB expanded per repository. Existing unverified cache entries are not overwritten. The runner exits nonzero for missing assertions, command failures, or nondeterministic output; **the current corpus is expected to exit 0**.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
```

The runner uses Python 3.11+ and the standard library only. Full per-check results and repeat timings are written to the specified JSON output; downloaded repositories and generated reports are not committed.
