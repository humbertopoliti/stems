Feature: one script at a time per stem
  A second run of a script of a stem waits for the first (`script.queued`)
  unless the script is `concurrent: true`.

  Scenario: a second run is queued behind the first
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run shop-api slow --json" in the background
    Then within 10s the events stream contains {"kind": "script.started", "data": {"script": "slow"}}
    When I run "stems run shop-api slow --json"
    Then the command succeeds
    And the JSON at "$.data.queued" equals true
    When I save the JSON at "$.data.run_id" as "second"
    Then the events stream contains {"kind": "script.queued", "data": {"script": "slow", "run_id": "${var:second}"}} before {"kind": "script.finished", "data": {"script": "slow"}}
    And the events stream contains {"kind": "script.finished", "data": {"script": "slow"}} before {"kind": "script.started", "data": {"script": "slow", "run_id": "${var:second}"}}
    And within 5s the background command exits with code 0

  Scenario: concurrent scripts overlap
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run shop-api overlap --json -- wait" in the background
    Then within 10s the events stream contains {"kind": "script.started", "data": {"script": "overlap", "args": ["wait"]}}
    When I run "stems run shop-api overlap --json -- release"
    Then the command succeeds
    And the JSON at "$.data.queued" equals false
    And within 10s the background command exits with code 0
    And the events stream does not contain {"kind": "script.queued"}
