# Working in this repo

## Changelog

Keep `CHANGELOG.md` up to date as part of every worthwhile change: a fix, a feature, a change in
behaviour, a new tool. Leave out what a user of Diggle or the Advisor would never notice, such as
comment-only edits, formatting and test-only refactors.

- Add entries under the top heading, `## [Unreleased]`, sorted by what they touch: `### Advisor`
  (the window and its table) or `### Combat word selection` (scoring under `#### Damage`, ordering
  under `#### Ranking`; Diggle and the Advisor share both). A fix sits beside the features, its
  wording saying what was wrong. Older releases keep their Added/Changed/Fixed headings.
- Write for the person using the tool: what changed for them, then where it lives in the code if
  that helps.
- **Cutting a release:** rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD`, add a fresh empty
  `## [Unreleased]` above it, and update the comparison links at the bottom of the file.
