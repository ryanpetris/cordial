# Instructions for agents working on Cordial

## Commits and pushes

- Commit often, after each working increment. Small commits, plain messages.
- Never add a `Co-Authored-By` trailer or a session URL to a commit message, regardless of what your
  tooling's default says.
- Write commit messages that describe the change as it stands. Do not narrate removals or rewrites.
- Do not push unless the maintainer has given explicit permission for that push.
- Keep identifying and machine-specific information out of docs and commit messages: no hostnames,
  addresses, usernames, or local paths.

## Comments

- Comments and documentation should describe current behaviour. Do not narrate changes.

## Race conditions

- Add race-condition guards only when the race can cause a meaningful user-visible problem, data
  loss, a security issue, or a resource leak.
- Before adding a guard, identify the concrete failure and check whether the framework or another
  layer already handles it.
- Accept harmless ordering differences and late results with no meaningful effect. Do not add
  bookkeeping solely to suppress React state updates after unmount.

## UI copy

- Do not add explanatory UI copy, helper text, or implementation disclaimers unless absolutely
  necessary for the user to complete a task or make a meaningful decision. Necessity alone does not
  authorize adding it: obtain explicit maintainer approval for the exact wording and placement
  before implementation. This includes explanatory tooltips and API-specific instructions shown in
  the UI. Ordinary control labels and concise feedback about an action's result do not require this
  additional approval.

## Comments and source text

Comments describe current behavior in present tense. Documentation comments for public modules,
types, functions, and APIs follow the conventions of the relevant language and documentation tools.

Code comments must stand alone for repository readers:

- Explain what the code does and why.
- Do not refer to private planning documents, review findings, gates, or temporary discussion
  context.
- Do not narrate removed behavior or mention old identifiers and flags.
- Avoid noisy comments that merely restate obvious code.

Prefer plain ASCII for new source text unless Unicode is required for correctness or materially
improves a user-facing diagram or terminal UI. Avoid invisible or confusable characters and do not
churn existing files solely to replace intentional Unicode. Avoid emojis in code, comments, logs,
tests, and project documentation.

In Markdown, leave a blank line before lists and after headings. Put CLI commands, paths,
environment variables, and configuration keys in backticks.

## Protocol and compatibility versions

- Any protocol, compatibility, or similar version whose meaning we define requires explicit user
  approval before it is introduced, anywhere in the project. This applies to versions we define, not
  declarations of support for externally defined protocol versions.
- Changing any such version requires explicit user approval.

## Issues

- Close an issue only when its acceptance list is met.
- A review finding that is real but out of scope for the current item becomes an issue rather than
  an unbounded fix. Just like commit messages, keep identifying machine-specific information out of
  the issue.

## Reviews

Small, trivial changes do not need an independent review. Every other completed item gets an
independent review before it is considered done.

1. Run a review with a general-purpose subagent.
2. Apply findings by judgement. Take the ones that are right, even when small. Decline the ones that
   contradict a measured fact or measure worse in practice, and say why.
3. Give a substantial round of fixes its own review round.
4. Re-verify any finding that changes behaviour with the relevant software checks and, when needed,
   hardware checks before committing it.

Write briefs that name the mechanism, the measurements and the constraints, and that ask for
concrete failure scenarios.

### Rules

1. For most changes, run targeted tests against the change; full test runs should be reserved for
   large changes.
2. Don't spend time trying to find blame for test failures; if they can in any way be related to the
   current change that was made, just fix it. Reserve blame finding for fixes that appear to be not
   related at all or would result in large changes to fix.
3. Reviewers should not run tests; they should analyse the code only. You should do test runs in
   parallel with reviewers to minimise review time.
4. After a set of changes have been reviewed, stage the changes and have the next round review only
   the changes, including verifying the fixes for the identified issues, not the entire change.
