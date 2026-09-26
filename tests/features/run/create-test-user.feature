@FR-SC-2 @FR-SC-7
Feature: run a custom stem script with validated arguments
  `stems run <stem> <script> -- <args>` validates the arguments against the
  script's `args` schema, passes them as `--name value` flags (and
  `STEMS_ARG_<NAME>` env) and tags the script's output with its name.
  hello-shop's `create-test-user` (copied into the fixture) POSTs to the
  running shop-api.

  Scenario: create-test-user creates a user on the running shop-api
    Given the fixture workspace "run-scripts"
    When I run "stems up shop-api --detach --json"
    Then the command succeeds
    When I run "stems run shop-api create-test-user --json -- --email a@b.c"
    Then the command succeeds
    And the JSON at "$.data.exit" equals 0
    And the JSON at "$.data.attempts" equals 1
    And the JSON at "$.data.argv" equals ["--email", "a@b.c", "--role", "admin"]
    And the JSON at "$.data.tail[0]" equals "create-test-user: ok (email=a@b.c, role=admin)"
    When I run "stems logs shop-api --script create-test-user --json"
    Then the command succeeds
    And the JSON at "$..text" contains "create-test-user: ok (email=a@b.c, role=admin)"
    And the JSON at "$..tag" contains "create-test-user"
    And within 5s the stem log file "shop-api/current.log" contains "created user a@b.c role admin"
    And the events stream contains {"kind": "script.started", "stem": "shop-api", "data": {"script": "create-test-user", "attempt": 1}} before {"kind": "script.finished", "stem": "shop-api", "data": {"script": "create-test-user", "ok": true}}
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a missing required argument is rejected before anything runs
    Given the fixture workspace "run-scripts"
    When I run "stems run shop-api create-test-user --json"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_ARGS_INVALID"
    And the JSON at "$.errors[0].details.arg" equals "email"
    And the error message contains "--email"

  Scenario: a value outside an enum is rejected
    Given the fixture workspace "run-scripts"
    When I run "stems run shop-api create-test-user --json -- --email a@b.c --role superuser"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_ARGS_INVALID"
    And the JSON at "$.errors[0].details.arg" equals "role"
    And the JSON at "$.errors[0].details.reason" contains "superuser"
    And the JSON at "$.errors[0].hint" contains "admin|user"

  Scenario: an unknown argument and an unknown script are errors
    Given the fixture workspace "run-scripts"
    When I run "stems run shop-api create-test-user --json -- --email a@b.c --admin"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_ARGS_INVALID"
    And the JSON at "$.errors[0].details.arg" equals "admin"
    When I run "stems run shop-api no-such-script --json"
    Then the exit code is 2
    And the JSON error has code "SCRIPT_NOT_FOUND"
    When I run "stems run no-such-stem ping --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
