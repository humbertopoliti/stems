@FR-CL-3
Feature: per-stem `requires:` with versions read from the codebase
  Fixture requires-from: stems `web`, `py`, `lts` (and a disabled `off`)
  declare `requires: { <tool>: { from: <file> } }`. The codebase has
  `.nvmrc` v20.11.1, `package.json` (packageManager pnpm@9.1.0, engines.node
  >=18) and `.tool-versions` (nodejs 20.11.1, python 3.12.1). Fake tools on
  PATH make the installed versions deterministic.

  Scenario: versions derived from .nvmrc, packageManager and engines are satisfied or not
    Given the fixture workspace "requires-from"
    And a fake tool "node" on PATH that runs "echo v20.11.1"
    And a fake tool "pnpm" on PATH that runs "echo 9.0.0"
    And a fake tool "yarn" on PATH that runs "echo 20.1.0"
    And a fake tool "python3" on PATH that runs "echo Python 3.12.1"
    When I run "stems doctor --json"
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'requires.web.node'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'requires.web.node'].message" contains "(from .nvmrc)"
    And the JSON at "$.data.checks[?@.id == 'requires.web.pnpm'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'requires.web.pnpm'].details.required" equals "9.1.0"
    And the JSON at "$.data.checks[?@.id == 'requires.web.pnpm'].details.found" equals "9.0.0"
    And the JSON at "$.data.checks[?@.id == 'requires.web.pnpm'].details.from" equals "package.json#packageManager"
    And the JSON at "$.data.checks[?@.id == 'requires.web.pnpm'].details.stem" equals "web"
    And the JSON at "$.data.checks[?@.id == 'requires.web.yarn'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'requires.web.yarn'].details.required" equals ">=18"

  Scenario: the first existing file of a from list is used
    Given the fixture workspace "requires-from"
    And a fake tool "python3" on PATH that runs "echo Python 3.12.1"
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'requires.py.python3'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'requires.py.python3'].details.from" equals ".tool-versions"
    And the JSON at "$.data.checks[?@.id == 'requires.py.python3'].details.required" equals "3.12.1"

  Scenario: a version that cannot be derived fails with a clear message
    Given the fixture workspace "requires-from"
    And a fake tool "node" on PATH that runs "echo v20.11.1"
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'requires.lts.node'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'requires.lts.node'].message" contains "cannot derive a version from `lts/*` in .nvmrc"

  Scenario: a disabled stem's requires are not checked
    Given the fixture workspace "requires-from"
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'requires.off.ruby']" does not exist

  Scenario: validate reports the same errors at the stem's path, and --skip-requires skips them
    Given the fixture workspace "requires-from" with its original ports
    And a fake tool "node" on PATH that runs "echo v20.11.1"
    And a fake tool "pnpm" on PATH that runs "echo 9.0.0"
    And a fake tool "yarn" on PATH that runs "echo 20.1.0"
    And a fake tool "python3" on PATH that runs "echo Python 3.12.1"
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "TOOL_VERSION" and path "stems.web.requires.pnpm"
    And the JSON error has code "TOOL_VERSION" and path "stems.lts.requires.node"
    And the JSON at "$.errors[?@.path == 'stems.web.requires.node']" does not exist
    And the JSON at "$.errors[?@.path == 'stems.off.requires.ruby']" does not exist
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
