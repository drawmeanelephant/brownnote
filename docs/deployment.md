# Deployment and deploy observability

There is no long-running production service. Two things are deployed, both
from CI, and this page says where to look after each one.

## 1. Documentation site (GitHub Pages)

- **What ships:** the Boris/Zig build of `docs/site/` — the field manual at
  <https://drawmeanelephant.github.io/brownnote/>.
- **When:** every push to `main` (and manual dispatch on `main`). Pull
  requests build and validate but never deploy.
- **Where to check the deploy:**
  - Actions run: `gh run list --workflow=docs.yml --limit 5`
  - Deploy history: `gh api repos/drawmeanelephant/brownnote/deployments`
  - Live site: <https://drawmeanelephant.github.io/brownnote/>
- **How to verify impact:** load the site, follow one nested guide page, use
  search for `--effect`, and check that the nav renders without JavaScript.
  A deploy that uploaded an empty artifact shows up as a 404 at the root.
- **Rollback:** revert the offending commit on `main`. The next push rebuilds
  and republishes; there is no separate rollback command.

## 2. Native test package and draft release

- **What ships:** an unsigned macOS arm64 tar.gz of `brownnote-native`, its
  `.sha256`, `.smoke.json`, and `build-record.json` — attached to a **draft**
  GitHub Release. Nothing is published to a registry, signed, or notarized.
- **When:** a maintainer pushes a `v<version>` tag (`.github/workflows/release.yml`).
  PRs that touch `native/**` produce the same artifact for inspection
  (`.github/workflows/native-package.yml`).
- **Where to check:**
  - `gh run list --workflow=release.yml`
  - `gh release view <tag>` (draft assets and the embedded notes)
  - The `.smoke.json` asset records the exact checks the binary passed.
- **How to verify impact:** download the archive, verify
  `shasum -a 256 -c <archive>.sha256`, run `brownnote-native --version`, then
  `--list-pedals` and `--check-audio` from the unpacked directory.
- **Rollback:** release assets are never replaced. If a draft is wrong,
  delete the draft with `gh release delete <tag> --yes` and fix forward with
  a new tag. A published test artifact is not recalled.

## 3. Alerting and incident flow

`.github/workflows/incident.yml` watches the `ci`, `native`, `native-package`,
`Documentation`, `release`, and `codeql` workflows on `main`. A failed run
creates (or updates) an issue labelled `incident` and `P1` with the failing
job, commit, and run URL. A recovered run comments on the open issue and
closes it.

If the repository has a Slack or PagerDuty webhook configured as the
`ALERT_WEBHOOK_URL` secret, the same workflow posts a one-line alert there.
No webhook is required; the issue is the durable channel.

## 4. Runtime signals

- CLI: `BROWNNOTE_LOG=info` gives structured `key=value` lines with a trace
  id, and `BROWNNOTE_PROFILE=1` prints per-phase timings at exit.
- Studio: `/metrics` exposes request counters in Prometheus text format,
  `/healthz` reports liveness and the enabled feature flags, and every
  response carries `X-Request-ID` matching the structured request log.
- Native: `--check-audio` prints callback duration, observed frames, and
  presentation lead; `make native-render-timeout` stresses the render
  deadline.

There are no hosted dashboards. The Actions run page is the deploy dashboard;
the links above are the canonical entry points.
