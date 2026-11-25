#!/bin/bash
# Checks that the public releases repo ($RELEASES_REPO) can take a release
# with the token in $GH_TOKEN (the RELEASES_TOKEN secret): the token is set
# and can read the repo, the repo is public (the app reads its update
# manifest without a login) and has a commit (a release needs one to tag),
# and the token may create releases there: a draft release, which creates
# no tag, made and deleted at once. Used by the release workflow's guard,
# before anything is built, and by the manual `Release setup check`
# workflow. Setup: docs/release-runbook.md.
set -euo pipefail

: "${RELEASES_REPO:?RELEASES_REPO is not set}"

if [ -z "${GH_TOKEN:-}" ]; then
  echo "::error::The RELEASES_TOKEN secret is not set (see docs/release-runbook.md)."
  exit 1
fi
PRIVATE="$(gh api "repos/$RELEASES_REPO" --jq .private)" \
  || { echo "::error::RELEASES_TOKEN cannot read $RELEASES_REPO."; exit 1; }
if [ "$PRIVATE" != "false" ]; then
  echo "::error::$RELEASES_REPO must be public: the app reads its update manifest without a login."
  exit 1
fi
# A release needs a commit to tag; an empty repo has none.
gh api "repos/$RELEASES_REPO/commits?per_page=1" > /dev/null \
  || { echo "::error::$RELEASES_REPO has no commits: initialize it with a README."; exit 1; }
# Any token can read a public repo, so only a write shows that this one may
# publish: a draft release (which creates no tag), deleted again at once.
PROBE="$(gh api -X POST "repos/$RELEASES_REPO/releases" \
  -f tag_name="release-guard-${GITHUB_RUN_ID:-local}" -F draft=true --jq .id)" \
  || { echo "::error::RELEASES_TOKEN cannot create releases on $RELEASES_REPO (it needs Contents: Read and write there)."; exit 1; }
gh api -X DELETE "repos/$RELEASES_REPO/releases/$PROBE" \
  || echo "::warning::Could not delete the guard's draft release $PROBE on $RELEASES_REPO; delete it by hand."
echo "$RELEASES_REPO is public, has a commit, and RELEASES_TOKEN may create releases there."
