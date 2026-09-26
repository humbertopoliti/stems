@FR-LC-4
Feature: Attached mode tears everything down on exit
  `stems up` without `--detach` stays attached, streaming progress. On
  Ctrl-C (SIGINT), SIGTERM, SIGHUP or EOF on an interactive stdin it runs
  `down --all` with a deadline and the daemon exits. `stems attach` follows
  a running daemon; leaving it stops nothing.

  Scenario: SIGINT to an attached up stops every stem and the daemon
    Given the fixture workspace "process-shop"
    When I run "stems up --json" in the background
    Then within 15s the background command's output contains [{"kind": "up.finished"}, {"ok": true, "data": {"ok": true}}] in order
    And within 5s the stem "shop-web" is "healthy"
    When the background command is stopped
    Then within 5s the background command exits with code 0
    And within 5s the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: attach follows events and detaching leaves the stems running
    Given the "minimal" workspace is up
    When I run "stems attach --json" in the background
    And I run "stems restart echo-svc --json"
    Then the command succeeds
    And within 5s the background command's output contains [{"kind": "stem.state", "stem": "echo-svc", "to": "stopping"}, {"kind": "stem.state", "stem": "echo-svc", "to": "healthy"}] in order
    When the background command is stopped
    Then within 5s the background command exits with code 0
    And within 2s the stem "echo-svc" is "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
