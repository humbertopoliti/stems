@FR-SC-8
Feature: script retries with backoff
  `retries: N` reruns a failed script up to N more times, waiting 500 ms,
  1 s, 2 s … (capped at 10 s) in between. Every attempt emits
  `script.started` / `script.finished` with `data.attempt`.

  Scenario: a script that fails twice succeeds on its third attempt
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run shop-api flaky --json"
    Then the command succeeds
    And the JSON at "$.data.attempts" equals 3
    And the JSON at "$.data.tail[0]" equals "flaky attempt 3"
    And the events stream contains {"kind": "script.finished", "data": {"script": "flaky", "attempt": 2, "ok": false}} before {"kind": "script.started", "data": {"script": "flaky", "attempt": 3}}
    And within 2s the events stream contains {"kind": "script.finished", "data": {"script": "flaky", "attempt": 3, "ok": true}}
    And the events stream does not contain {"kind": "script.started", "data": {"script": "flaky", "attempt": 4}}
    When I run "stems logs shop-api --script flaky --json"
    Then the JSON nodes at "$[*].text" equal ["flaky attempt 1", "flaky attempt 2", "flaky attempt 3"]

  Scenario: retries run out: SCRIPT_FAILED with the attempts made
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run shop-api always-fails --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_FAILED"
    And the JSON at "$.errors[0].details.exit" equals 3
    And the JSON at "$.errors[0].details.attempts" equals 2
    And the JSON at "$.data.attempts" equals 2
    And the JSON at "$.data.ok" equals false
