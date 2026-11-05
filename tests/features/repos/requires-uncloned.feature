@FR-CL-3 @FR-WS-3
Feature: requires read from a git codebase that is not cloned yet
  A per-stem `requires: { <tool>: { from: <file> } }` of a git codebase
  cannot be read before `up` clones it: `validate` skips it with a
  `REQUIRES_NOT_CHECKED` warning (exit 0), `doctor` reports `warn`, and `up`
  clones and starts the stem. Once cloned, the entry is checked as usual.

  Scenario: validate warns, doctor warns, up clones and starts, then validate checks it
    Given the fixture workspace "git-requires"
    And a bare git repository "app" made from "tests/fixtures/workspaces/git-requires/repo"
    And a local override setting stems.app.codebase.git=${var:app_url}
    And a fake tool "frob" on PATH that runs "echo 1.2.3"
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[0].code" equals "REQUIRES_NOT_CHECKED"
    And the JSON at "$.data.warnings[0].path" equals "stems.app.requires.frob"
    And the JSON at "$.data.warnings[0].details.from" equals [".frob-version"]
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'requires.app.frob'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'requires.app.frob'].message" contains "codebase not cloned yet"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" contains "app"
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.warnings" equals []
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'requires.app.frob'].status" equals "ok"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
