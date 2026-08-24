# Contributing

Thank you for looking. Please read this before spending time on a change.

## Pull requests are unlikely to be reviewed promptly

This is a pet project, and it is under **high churn** — the internals move daily, whole modules get
reshaped when a live run disagrees with them, and design rules get overturned by a single evening's
evidence. A branch that looks correct today can be arguing with code that no longer exists by the
time anyone reads it.

So: pull requests are welcome to exist, and you should not expect one to be reviewed or merged in
any particular timeframe, or at all. That is a statement about the maintainer's capacity and the
project's pace, not about the quality of your work. If you want a change for yourself, fork it —
that is genuinely the best use of this repo, and no permission is needed.

## Discussion is very welcome

The **Sternly Worded Adventures Discord** is the right place: <https://discord.gg/tBDWhB7BCm>

Especially worth raising there:

- **A run that failed in a way the log does not explain.** Attach `spike-run-*.md` and the
  matching `.log`. Those two files are the project's real currency.
- **A claim in a doc comment that is wrong about the game.** Citations of the form
  `file.lua:123-456` point into the game source and are meant to be checked. If one does not say
  what the comment says it says, that is worth more than a patch.
- **Anything about the game's own behaviour** — the console output, an undocumented screen, what a
  button is gated on. This project is mostly an accumulation of that kind of knowledge.

## If you do send a patch anyway

Keep it small and make it self-explaining, because it may sit for a while and will have to justify
itself to a reader with no memory of the conversation.

- `cargo fmt --all` and `cargo test` should both be clean. There is a `cargo fmt --all --check`
  pre-commit hook worth installing locally.
- **Put the reasoning in a doc comment, not the commit message.** That is the whole convention
  here: why a rule exists, what was tried and rejected, and the live failure that produced it,
  next to the code it governs.
- **Cite the game.** A claim about *Sternly Worded Adventures* should point at the Lua that makes
  it true. If you are inferring rather than citing, say so in the comment — that is a perfectly
  good state for a claim to be in, as long as it is labelled.
- **A guard needs a test that fails without it.** Several bugs here survived a test that passed
  against the bug, so a negative control is worth more than an assertion.

## Do not

- **Modify the game.** Nothing in this project changes *Sternly Worded Adventures*; it observes
  the running process and sends real input. A change that needs the game patched is out of scope.
- **Touch `%APPDATA%\SternlyWordedAdventures`.** That is a real Steam save. The sandbox is
  `%APPDATA%\LOVE\SternlyWordedAdventures`.
- **Read the answer out of the save.** The shrine word is in there and reading it is cheating; so
  is disabling the hurt vignette to make a screen easier to classify. The solver plays the game it
  can see.
