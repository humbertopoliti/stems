@FR-LC-3
Feature: start, stop and restart single stems
  `stop` refuses while running stems depend on the stem (HAS_DEPENDANTS)
  unless `--cascade`, which stops the dependants first. `start` also starts
  unstarted hard dependencies (`--no-deps` to skip them). `restart` is stop
  + start: a new process on the same ports.

  @error
  Scenario: stop with running dependants needs --cascade
    Given the fixture workspace "process-shop"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems stop shop-api --json"
    Then the exit code is 1
    And the JSON error has code "HAS_DEPENDANTS"
    And the JSON at "$.errors[0].details.dependants" equals ["shop-web"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "healthy"
    When I run "stems stop shop-api --cascade --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["shop-web", "shop-api"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='shop-api'].glyph" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='shop-api'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='shop-web'].state" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='shop-worker'].state" equals "healthy"
    And the JSON at "$.data.summary" equals {"healthy": 1, "degraded": 0, "failed": 0, "stopped": 2, "unknown": 0, "starting": 0}
    When I run "stems start shop-web --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["shop-api", "shop-web"]
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: restart gives a new pid on the same port
    Given the "minimal" workspace is up
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And I run "stems restart echo-svc --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
    And the JSON at "$.data.stems[0].ports[0].port" equals ${port:18090}
    And within 2s none of the pids ${var:pid} is alive
    When the chaos endpoint "logs?n=1" is called on "echo-svc"
    Then the chaos response status is 200
    When I run "stems down --all --json"
    Then the command succeeds

  @error
  Scenario: restart --build lands with deliverable 16
    Given the "minimal" workspace is up
    When I run "stems restart echo-svc --build --json"
    Then the exit code is 1
    And the JSON error has code "NOT_IMPLEMENTED"
    When I run "stems down --all --json"
    Then the command succeeds
