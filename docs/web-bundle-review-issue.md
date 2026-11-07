# Issue 草稿：Web bundle 威胁模型评审

> 本地没有 `gh` CLI / GitHub token，无法自动创建 issue。把下面内容原样粘贴到
> GitHub 新建 issue，标题与标签按建议填写；创建后把链接补回
> `docs/web-bundle-threat-model.md` 的 Review log。

**建议标题**

`Web bundle threat model review: 8 open questions (needs-decision)`

**建议标签**

`security`, `design`, `needs-decision`

**正文**

---

This issue is the external review entry point for the planned Web bundle (frontend
OTA) path. The threat model is in [`docs/web-bundle-threat-model.md`](docs/web-bundle-threat-model.md).

**Status:** design review only. No replacement code will be written until the
questions below have a decision.

Please comment with:

- the question number,
- your proposed answer,
- any additional attack surface we missed.

### Baseline: `tauri-plugin-hot-update`

Before answering the questions, note that `tauri-plugin-hot-update` already
documents signed bundles, atomic rollback, and a `staged → booting → committed`
pointer with no timers. This design is therefore **not differentiated by
"safer rollback"**. The table below marks where we align and where we still need
a specific argument.

| Capability | hot-update | this design | Position |
|---|---|---|---|
| Signed bundle | yes | manifest signature covers URL + hash + version | aligned; extra argument is URL coverage, not just hash |
| Three-state rollback | yes, no timers | separate `staged → activated → confirmed` | aligned on no-timer; separate state because web does not restart |
| Hardened extraction | yes | reuse `core::bundle` path checks | aligned |
| Semantic version monotonicity | not documented | yes, reuse native `is_newer_than` / `min_version` | **candidate differentiator** |
| Asset scope isolation | not documented | open question Q1 | **candidate differentiator** |
| Retention policy | not documented | N=2 proposal, Q7 | **candidate differentiator** |
| One engine for native + web | no | planned | structural, but unproven |

Decision rule: if only the "aligned" rows hold, this path is lower priority
than cross-platform real-filesystem validation. It stays worth doing only if at
least 2–3 "candidate differentiator" rows survive review with a concrete,
testable argument.

### Open questions

1. **Asset scope**
   Can Tauri v2's asset protocol scope be switched at runtime? If it is global,
   can a bundle path and user files share the same scope? Candidate: fixed root
   `web/`, one subdirectory per version, scope only `web/`.

2. **Bundle location**
   App data (survives cleanup tools, may be backed up) vs cache (lighter, may be
   deleted and break rollback). Candidate: app data with N=2 retained versions;
   cache only for in-flight downloads.

3. **State pointer**
   Reuse the native `staged → booting → committed` file, or a separate
   `staged → activated → confirmed` file? Candidate: separate files and
   separate `Diagnosis` fields; the lifecycles differ (no process restart).

4. **Who writes `confirmed`**
   Rust host, frontend JS, or both? If JS can write it, an XSS can pin a bad
   bundle. Candidate: Rust only; frontend cannot write activation state.

5. **Signature granularity**
   One manifest signature covering URL + hash + version, or an additional
   bundle-level signature? Candidate: manifest-only in phase 1.

6. **Web version monotonicity**
   Can the web bundle have its own `web_version` independent of the app version?
   How is an old bundle manifest replayed? Candidate: separate `web_version`
   with the same semantic monotonicity and `min_version` semantics.

7. **Retention and disk budget**
   How many old bundles are kept? What is the per-bundle size cap? Candidate:
   2 confirmed versions, shared disk budget with native patches.

8. **CSP and cache invalidation**
   How is the WebView cache invalidated after activation? Are content-hashed
   filenames required? Candidate: hashed filenames + Rust-injected entry path;
   strict CSP, no `unsafe-eval`.

### Exit criteria

- Every question has an answer recorded in the threat model.
- The threat model's "Review log" records at least one external comment.
- Until then, the Web bundle path is marked experimental and is not advertised
  as a production feature.

---

**创建后回填**

```text
Review log:
- <date> — GitHub issue <URL> — <summary/decision>
```
