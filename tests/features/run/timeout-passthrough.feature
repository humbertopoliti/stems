@FR-SC-7 @FR-SC-8
Feature: passthrough arguments, STEMS_ARG_* env and timeouts
  A script without `args` receives everything after `--` untouched; a
  script with a schema gets `--name value` flags (defaults applied, sorted by
  name) and `STEMS_ARG_<NAME>` variables. `timeout:` bounds every run.

  Scenario: a script without a schema gets the raw arguments
    Given the fixture workspace "run-scripts"
    When I run "stems run shop-api echo-args --json -- --anything goes -x 'a b'"
    Then the command succeeds
    And the JSON at "$.data.argv" equals ["--anything", "goes", "-x", "a b"]
    And the JSON at "$.data.tail[0]" equals "argv: --anything goes -x a b"

  Scenario: schema'd arguments arrive as flags and STEMS_ARG_* env
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run shop-api env-args --json -- --email a@b.c --verbose"
    Then the command succeeds
    When I run "stems logs shop-api --script env-args --json"
    Then the JSON at "$..text" contains "argv: --count 2 --email a@b.c --verbose true"
    And the JSON at "$..text" contains "env: email=a@b.c count=2 verbose=true"

  Scenario: a script past its timeout is killed
    Given the fixture workspace "run-scripts"
    When I run "stems run shop-api short --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_FAILED"
    And the JSON at "$.errors[0].details.reason" equals "timeout"
    And the JSON at "$.data.timed_out" equals true
