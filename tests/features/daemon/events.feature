@FR-CL-4
Feature: The event stream
  `stems events --json` prints the daemon's events as NDJSON (one object
  per line, no envelope); `-f` keeps streaming until the daemon stops.

  Scenario: a follower sees the daemon start and stop, in order
    Given the "minimal" workspace
    And the daemon is started
    When I run "stems events -f --json" in the background
    Then within 5s the background command's output contains [{"kind": "daemon.started", "actor": "daemon"}] in order
    When I run "stems daemon stop --json"
    Then the command succeeds
    And within 5s the background command's output contains [{"kind": "daemon.started"}, {"kind": "daemon.stopping", "reason": "shutdown requested"}, {"kind": "daemon.stopped"}] in order
    And within 5s the background command exits with code 0

  Scenario: --since replays only newer events
    Given the "minimal" workspace
    And the daemon is started
    Then within 5s the events stream contains {"kind": "workspace.loaded", "data": {"name": "minimal"}}
    When I run "stems events --json --since 0"
    Then the command succeeds
    And the JSON at "$[0].kind" equals "daemon.started"
    And the JSON at "$[0].seq" equals 1
    And the JSON at "$[1].kind" equals "workspace.loaded"
    When I run "stems events --json --since 1"
    Then the command succeeds
    And the JSON at "$.kind" equals "workspace.loaded"
    And the JSON at "$.seq" equals 2

  Scenario: a follower with --since replays, then stops on Ctrl-C
    Given the "minimal" workspace
    And the daemon is started
    Then within 5s the events stream contains {"kind": "workspace.loaded"}
    When I run "stems events -f --json --since 1" in the background
    Then within 5s the background command's output contains [{"kind": "workspace.loaded", "seq": 2}] in order
    When the background command is stopped
    Then within 1s the background command exits with code 0
