@FR-HS-1 @error
Feature: a stem that never becomes healthy fails with START_TIMEOUT
  A stem still `starting` after `health.start_timeout` is `failed` with
  START_TIMEOUT (the last probe error in the message) and its process group
  is stopped. `up` reports it in `failed`; with no stem ready it exits 1 (3
  when others are ready, see lifecycle/partial-failure.feature).

  Scenario: a 5 s boot with a 1 s start_timeout
    Given the "minimal" workspace
    And a local override setting stems.echo-svc.env.SHOP_SLEEP_START="5"
    And a local override setting stems.echo-svc.health.start_timeout=1s
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON at "$.data.ready" equals []
    And the JSON at "$.data.failed[0].stem" equals "echo-svc"
    And the JSON at "$.data.failed[0].error.code" equals "START_TIMEOUT"
    And the JSON at "$.data.failed[0].error.details.start_timeout_ms" equals 1000
    And the JSON error has code "START_TIMEOUT"
    And the error message contains "start_timeout of 1s"
    And the last command took less than 4000 ms
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "failed"
    And the JSON at "$.data.stems[0].pid" equals null
    And the JSON at "$.data.stems[0].error.code" equals "START_TIMEOUT"
    # `down` has nothing to stop: the timed-out process was already killed.
    When I run "stems down --all --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals []
    And no process from the workspace's process groups is alive
