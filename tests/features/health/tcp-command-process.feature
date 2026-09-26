@FR-HS-1
Feature: tcp, command and process health probes
  One scenario per probe type against echo-svc (shop-api): `tcp` (the
  minimal workspace's own check) connects to the port, `command` runs a
  shell command in the stem's codebase with its environment (exit 0 =
  healthy; its output reaches the stem log tagged `health` only when it
  fails or its outcome changes), `process` only checks the process is alive.

  Scenario: tcp connects to the stem's port
    Given the "minimal" workspace
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems health --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].name" equals "echo-svc"
    And the JSON at "$.data.stems[0].type" equals "tcp"
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].results[0].ok" equals true
    And the JSON at "$.data.stems[0].results[0].detail" contains "connected to localhost:"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].health.type" equals "tcp"
    And the JSON at "$.data.stems[0].health.consecutive_failures" equals 0
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: command runs a check with the stem's environment
    Given the "minimal" workspace
    And a local override setting stems.echo-svc.health.type=command
    And a local override setting stems.echo-svc.health.command=python3 -c 'import os, urllib.request as u; u.urlopen("http://127.0.0.1:%s/healthz" % os.environ["PORT"], timeout=1)'
    And a local override setting stems.echo-svc.health.timeout=2s
    And a local override setting stems.echo-svc.health.interval=200ms
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems health echo-svc --json"
    Then the JSON at "$.data.stems[0].type" equals "command"
    And the JSON at "$.data.stems[0].results" contains {"ok": true, "detail": "exit 0"}
    And the events stream does not contain {"kind": "script.started", "data": {"script": "health"}}
    And within 3s the stem log file "echo-svc/current.log" contains "health check passed"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: process only needs the process to be alive
    Given the "minimal" workspace
    And a local override setting stems.echo-svc.health.type=process
    And a local override setting stems.echo-svc.health.interval=200ms
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems health echo-svc --json"
    Then the JSON at "$.data.stems[0].type" equals "process"
    And the JSON at "$.data.stems[0].results" contains {"ok": true, "detail": "alive"}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
