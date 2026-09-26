@FR-WS-2 @error
Feature: script files live in the integration repo
  A `file:` script is resolved relative to the integration repo (the
  directory of stems.yaml) and must exist inside it: a missing file is
  SCRIPT_NOT_FOUND, one outside the repo (`..`, absolute path, symlink out)
  SCRIPT_OUTSIDE_WORKSPACE. Both are validation errors (exit 2), so `up`
  starts nothing; the script runner checks again before every run.

  Scenario: validate reports a missing and an outside script file
    Given the fixture workspace "script-files" with its original ports
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_NOT_FOUND" and path "stems.echo-svc.scripts.setup"
    And the JSON error has code "SCRIPT_OUTSIDE_WORKSPACE" and path "stems.echo-svc.scripts.seed"
    And the error message contains "scripts/nope.sh"

  Scenario: up refuses to start with a broken script file
    Given the fixture workspace "script-files" with its original ports
    When I run "stems up --detach --json"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_NOT_FOUND"
    And the JSON at "$.errors[0].details.errors[?@.code == 'SCRIPT_OUTSIDE_WORKSPACE'].path" equals "stems.echo-svc.scripts.seed"
