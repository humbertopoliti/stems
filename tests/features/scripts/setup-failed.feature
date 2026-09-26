@FR-SC-1 @error
Feature: a failing setup fails its stem with SETUP_FAILED
  A `setup` that exits non-zero fails the stem (state `failed`) before its
  process is started. The error carries the exit code and the last 20 lines
  of the script's output. Other stems still come up (`--no-fail-fast`), so
  `up` exits 3.

  Scenario: setup exits 7
    Given the fixture workspace "scripts-fail"
    When I run "stems up --detach --no-fail-fast --json"
    Then the exit code is 3
    And the JSON at "$.data.ready" equals ["ok-svc"]
    And the JSON at "$.data.failed[0].stem" equals "bad-svc"
    And the JSON error has code "SETUP_FAILED"
    And the JSON at "$.errors[0].details.exit" equals 7
    And the JSON at "$.errors[0].details.script" equals "setup"
    And the JSON at "$.errors[0].details.tail" contains "fatal: cannot prepare"
    And the JSON at "$.errors[0].details.tail" contains "preparing"
    And the error message contains "setup of `bad-svc` failed (exited with status 7)"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='bad-svc'].state" equals "failed"
    And the JSON at "$.data.stems[?@.name=='bad-svc'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='ok-svc'].state" equals "healthy"
    And the events stream does not contain {"kind": "stem.state", "stem": "bad-svc", "to": "starting"}
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps" equals []
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
