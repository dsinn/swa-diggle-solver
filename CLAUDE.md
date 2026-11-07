# Working in this repo

## Changelog

Keep `CHANGELOG.md` up to date as part of every worthwhile change: a fix, a feature, a change in
behaviour, a new tool. Leave out what a user of Diggle or the Advisor would never notice, such as
comment-only edits, formatting and test-only refactors.

- Add entries under the top heading, `## [Unreleased]`, sorted into `Added`, `Changed` and `Fixed`
  as the file already does ([Keep a Changelog](https://keepachangelog.com/en/1.1.0/)).
- Write for the person using the tool: what changed for them, then where it lives in the code if
  that helps.
- **Cutting a release:** rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD`, add a fresh empty
  `## [Unreleased]` above it, and update the comparison links at the bottom of the file.
