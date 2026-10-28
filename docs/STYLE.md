# Writing the PolkaMeter docs

Rules for anyone editing `docs/`. This file is not part of the book.

## Page types

Every page is one of four kinds. Do not mix them on one page.

| Kind | Reader is… | The page… | Example |
| --- | --- | --- | --- |
| Tutorial | learning | walks one path from nothing to a result, with output at every step | Getting started |
| How-to | working | solves one task, steps only, no background | Run PolkaMeter in CI |
| Reference | looking something up | lists every element, flag or file in a fixed format | Plan reference |
| Explanation | trying to understand | explains why and how it works | How a run works |

A tutorial links to the reference for details. A reference page links to the explanation for reasons. Neither repeats the other.

## Rules

1. **Result first, internals later.** Say what the reader gets before how PolkaMeter does it.
   - Before: "`plugin install` reads the plugin's manifest and records its version and the BLAKE2 hash of its executable. Reinstall after rebuilding the plugin, because the hash changes."
   - After: "Install the plugin again after you rebuild it. PolkaMeter refuses to run a plugin whose file has changed since it was installed."

2. **Show real output after every command.** Paste it from an actual run. Cut long output with `...`. Never invent output.

3. **Example before reference.** Start a reference page with a complete, small example, then the tables.

4. **One idea per paragraph, two to four sentences.** If a sentence has more than one comma-separated list, split it.

5. **Define a term the first time you use it, or link to where it is defined.** Do not use internal names (`run.probeBudget`, `QueueSource`) in user pages.
   - Before: "For a plan without a `<load>`, it holds:"
   - After: "A plan without a load test (no `<load>` element) writes these files:"

6. **Say it directly.** Prefer "if X, Y" to passive constructions. Address the reader as "you".
   - Before: "A step that submits a transaction and times out may already have been accepted."
   - After: "If a submit step times out, the chain may still have accepted the transaction. PolkaMeter does not retry it."

7. **Plain words over notation.**
   - Before: "The statuses partition `sent`."
   - After: "Every sent transaction ends with exactly one status."

8. **Cut sentences that announce or summarise.** No "This page explains…", "In summary…", "Note that…", "It is important to…". No "simply", "just", "easy", "please", "let's".

9. **Warnings sit next to the step they apply to,** as a blockquote starting with **Warning:**. Use at most one per section.

10. **Commands are copyable.** Say which directory to run them from. Use `polkameter` as the command name; the tutorial shows once how to put it on `PATH`. Mark placeholders as `<like-this>`.

11. **Same words everywhere:** PolkaMeter (not Polkameter), plan, run, run directory, plugin, step, load test, transaction, verdict. Headings in sentence case.

12. **Only document what exists.** Do not document the desktop app. Do not describe planned features. The CLI (`polkameter`) and the library crates for plugins are on crates.io at 0.2.0. Install the CLI with `cargo install --locked polkameter`. Do not claim a crate is on crates.io until its release is published there.

## Reference entry format

For each element, command or file:

- one line saying what it is for;
- a table with name, type, required, default, limits;
- one or two short examples;
- a link to the explanation, if there is one.
