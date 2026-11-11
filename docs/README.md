# Design documents

These describe how the solver is put together. They are a map, not the territory: the reasoning
behind each rule, the live failure that produced it and the Lua that justifies it live in doc
comments next to the code (see [CONTRIBUTING.md](../CONTRIBUTING.md)). When a document here and a
doc comment disagree, the doc comment is right; please say so.

| Document | What it covers |
|---|---|
| [architecture.md](architecture.md) | The layers, the ways the solver sees the game, and the rules every layer shares |
| [observation.md](observation.md) | Reading the game: the console, the save, and the screen |
| [combat.md](combat.md) | Choosing and playing a word, and what the Advisor shares with the driver |
| [navigation.md](navigation.md) | The overworld map, the goal ladder, subworld crossings and loop guards |

Open work is tracked in [GitHub issues](https://github.com/dsinn/swa-diggle-solver/issues). A `#N`
in a commit message or a doc comment is an issue number.
