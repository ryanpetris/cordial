# Instructions for agents working on Cordial

## Definitions

- Dongle: The embedded system running the Cordial software, such as a Raspberry Pi Pico 2 or an
  ESP32-S3
- Device: A keyboard or mouse connected via Bluetooth to the Dongle
- Host: The computer that uses the emulated USB keyboard and/or mouse and runs the client software.

## Scope

Cordial is software that runs on a Dongle to connect to and translate Bluetooth Classic, BLE, and
Logitech HID++ Devices into a standard USB keyboard and mouse. The Host software does not provide
any functionality other than configuration of the Dongle as well as a small amount of reporting
such as battery percentage and charging status. Any feature that otherwise requires software
running on the Host to operate is out of scope. Any feature to directly control the Device other
than to configure it is out of scope. All settings that would be needed to configure the Device
upon connect must be stored on the Dongle.

## Logitech devices

- Evaluate individual controls, not whole HID++ feature IDs. A feature can contain
  Implemented, Planned, Not Planned, and Excluded operations.
- Normal operation must work with the Dongle alone. Host software may configure preferences and
  display status, but must not be required to provide the feature during use.
- User-facing writes configure ongoing Device behavior. Store preferences on the Dongle and reapply
  them when needed. One-shot Device commands and continuous software control are out of scope.
  Internal discovery and configuration operations needed for supported input translation are allowed.
- Configure only the current Device connection. Other paired hosts, their settings and metadata,
  host switching, and management of the Device's host slots are out of scope.
- Input translation must produce standard USB input. Fixed key mappings and simultaneous held key
  combinations are allowed. Running commands or processes, typing application names into menus,
  and timed key sequences are out of scope. Preserve simultaneous presses and releases.
- Do not implement diversion of standard Device behavior, such as mouse wheel scrolling, without
  a demonstrated reason. Preserve native HID reporting when it provides the required behavior.
  Before adding diversion, document the concrete missing capability or failure and why diversion
  is needed to address it. Protocol support alone is not a reason. Any diverted input must have
  complete Dongle-owned translation into standard USB input.
- Reporting must answer a useful user question. Keep useful status and settings information, place
  technical diagnostics on the Device's Diagnostics tab, and omit raw implementation data without a
  useful presentation.
- Expose operations only when supported by protocol evidence and the Device's advertised
  capabilities. Verify writes with the relevant readback or documented confirmation. Record missing
  documentation or hardware verification without treating it as a permanent scope exclusion.
  Use Planned only for operations selected for implementation; use Not Planned for operations
  without an implementation commitment. Resolving an unknown does not automatically make an
  operation Planned.
- Maintain [the Logitech feature inventory](docs/hidpp.md) when support or scope changes.
  Record the feature ID, description, implementation, status, and reasons for exclusions and
  unplanned operations. UI complexity is a consideration for each feature, not a blanket scope
  rule. Keep scope exclusions separate from implementation priorities and unresolved research,
  interface design, or hardware verification.

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

### Writing copy

Messages:

- Write complete sentences that name the device or adapter, say what happened, and give a next step
  when there is a useful one: "The adapter is busy. Try again when the current operation finishes."
- Put the statement and the next step in separate sentences, not joined with a semicolon.
- Name the failed action instead of a vague fragment: "The adapter couldn't save the change.", not
  "Couldn't send." Add the specific reason after it as its own sentence.
- Say "the adapter" for the Dongle, "this device" or the device's name for a Device, and "Cordial"
  for the app. Never show "dongle", internal command or field names, or raw codes.
- When an outcome is unknown, say so ("The adapter couldn't confirm whether the change was saved.")
  rather than reporting success or failure.
- Desktop and TUI messages start with a capital letter and end with a period. Error reasons that are
  inserted into other text, such as "Failed: {reason}", have no final period. CLI error reasons are
  lowercase clauses that follow "Error:" or a colon, such as "the request was too large for the
  adapter".
- Use the same words for the same thing in the desktop app, CLI and TUI, including labels such as
  "Logitech Features", "Pointer Speed {n}" and status words such as "Setting Up" and "Unsupported".

Labels, buttons and status:

- Use short Title Case noun or verb phrases without a final period.
- Hide a section or row with nothing to show rather than showing "None".

Placement:

- Device warnings, protocol details, link security, identifiers and a device's last connection error
  belong on the device's Diagnostics tab, never as banners or on the Details or Settings tabs.
- The result of an action the user just took appears next to that action, such as in the page's
  bottom bar, and clears when it no longer applies.

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
- Any change to the serial protocol between the Dongle and the Host software, including
  `proto/cordial.proto`, framing, and the documented behaviour of its messages, requires explicit
  maintainer approval before it is made.
- Any change to the Dongle's on-board storage layout requires explicit maintainer approval before
  it is made. This includes adding, removing, or renaming files and directories, changing paths,
  and changing the format, fields, or meaning of file contents, as documented in
  `docs/storage-format.md` and `docs/storage.schema.json`.
- Firmware downgrades are not supported. Saved data written by newer firmware does not need to
  stay readable by older firmware.

## Issues

- Close an issue only when its acceptance list is met.
- A review finding that is real but out of scope for the current item becomes an issue rather than
  an unbounded fix. Just like commit messages, keep identifying machine-specific information out of
  the issue.

## Required completion checks

Before any item is considered done, all suites below must pass for the completed changes.
Use targeted tests during development, then complete this checklist before reporting completion.
Passing results from the same work can be reused while their inputs remain unchanged. A skipped
fixture or a missing prerequisite is not a pass; resolve it or report the item as incomplete.
Keep this catalog aligned with `Makefile`, `desktop/package.json`, and
`.github/workflows/release.yml`. It lists suites and fixtures, not individual test cases.

| Suite or fixture | Required check and coverage |
| --- | --- |
| Rust workspace | `make check-rust`: unit, integration, and documentation tests for all workspace crates with all features, including the core, protocol, client, CLI/TUI, USB, BLE HID, BTstack, and record storage adapters. Includes the ESP storage fixture. |
| Rust static checks | `make check-rust`: workspace Clippy for all targets and features, with warnings treated as errors. |
| Native BTstack callbacks | `make check-rust`: `rust/tools/test_btstack.py` builds and runs the profiles, GATT, device information, layout, and saved-security C fixtures. |
| Generated desktop protocol | `make check-desktop`: regenerate and compare checked-in TypeScript protocol messages and setting keys. |
| Desktop type checks | `make check-desktop`: TypeScript checks for the desktop and shared packages. |
| Desktop and shared client tests | `make check-desktop`: all Vitest fixtures in `desktop/test` and `desktop/packages/*/test`, including desktop state, settings, serial communication, notifications, tray, server, client transports, framing, and keys. |
| Desktop garbage-collection fixtures | `npm --prefix desktop test -- --execArgv=--expose-gc`: enables the notification and USB watcher lifetime checks that otherwise skip when `global.gc` is unavailable. |
| Protocol compatibility | `make check-protocol`: key catalog validation and compatibility, Buf lint, and Buf breaking checks against `PROTOCOL_BASE` (the newest earlier release in the same series as `CORDIAL_VERSION`) when that base contains the protocol. |
| Repository tool fixtures | `make check-tools`: `tools/test_*.py`, covering versions, key catalog checks, and release archive validation. |
| Firmware and packaging tool fixtures | `make check-tools`: `rust/tests/test_*.py`, covering board configuration, firmware artifacts, flashing, dependency notices, packaging, native BLE, HID reports, and host terminal behavior. |
| Linux CLI/TUI terminal fixture | Build the CLI and install `pyte`, then run `CORDIAL_TEST_BINARY="$PWD/rust/target/release/cordial" python3 -m unittest discover -s rust/tests -p test_host_terminal.py`. The default tool run skips this fixture without these prerequisites. |
| ARM memory fixture | `make check-memory`: build development firmware for each Pico board and run the 32-bit allocation workloads in `rust/tests/memory-arm` under QEMU with that board's configuration, against its linked firmware's heap. |
| Firmware static checks | `make check-firmware-all`: firmware Clippy for `pico_w`, `pico2_w`, `waveshare_rp2350b_plus_w`, and `xiao_esp32s3`, each with `production` and `debug` profiles. |
| Firmware artifact validation | `make firmware-all PROFILE=production` and `make firmware-all PROFILE=debug`: link and package every board, including metadata, image layout, and storage-boundary checks in `rust/tools/build_firmware.py`. |
| Portable application packages | `make package-desktop package-cli package-web`; validate the desktop and CLI archives with `tools/check_release_archive.py` for the selected `CORDIAL_VERSION`. |
| Arch package fixture | Build both native packages with `make package-desktop-arch package-cli-arch`, then run `packaging/check-arch.sh` in a clean Arch container. Checks independent installation, native modules, package contents, Namcap, reinstallation, and removal. |
| Debian/Ubuntu package fixtures | Build both native packages with `make package-desktop-deb package-cli-deb DEB_DISTRIBUTION=...`, then run `packaging/check-deb.sh` in the matching clean container for `trixie`, `noble`, and `resolute`. Checks independent installation, native modules, package contents, Lintian, reinstallation, and removal. |
| Installed desktop smoke fixture | Install the Noble desktop package, then run `CORDIAL_EXECUTABLE=/opt/cordial-desktop/cordial-desktop xvfb-run -a dbus-run-session -- node desktop/scripts/smoke.mjs desktop/out/package-smoke` with the Chromium sandbox enabled. Exercises the main views and pairing with simulated adapters. |
| Release asset validation | When assembling a release, verify all expected application and board/profile archives and their checksums using the asset checklist in `.github/workflows/release.yml`. Publishing is not a check and still requires authorization. |

`make check` covers the Rust, desktop, protocol, and Python tool suites. It does not enable the
garbage-collection or terminal prerequisites, or run the memory, firmware matrix, package
installation, or installed desktop smoke checks. Run package installation fixtures only in
disposable environments, never against the developer's installed
applications. See [building documentation](docs/building.md) and the release workflow for setup
and the distribution matrix.

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

1. During development, run targeted tests against the change. Before completion, all
   [required completion checks](#required-completion-checks) must pass.
2. Don't spend time trying to find blame for test failures; if they can in any way be related to the
   current change that was made, just fix it. Reserve blame finding for fixes that appear to be not
   related at all or would result in large changes to fix.
3. Reviewers should not run tests; they should analyse the code only. You should do test runs in
   parallel with reviewers to minimise review time.
4. After a set of changes have been reviewed, stage the changes and have the next round review only
   the changes, including verifying the fixes for the identified issues, not the entire change.
