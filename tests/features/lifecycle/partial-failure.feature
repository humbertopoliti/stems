@FR-LC-1 @error
Feature: up with a stem that crashes at start
  A stem whose process exits before it is ready is `failed` with
  START_FAILED. `up` reports it in `failed`, keeps the stems that did start
  running, and exits 3 (partial). Fail-fast (the default) skips stems that
  had not started yet; `--no-fail-fast` only skips the failed stem's
  dependants. The `process-shop` layers are [shop-api, shop-worker] then
  [shop-web].

  Scenario: fail-fast: the crash skips the next layer
    Given the fixture workspace "process-shop"
    And a local override setting stems.shop-worker.env.SHOP_CRASH_ON_START="3"
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON at "$.ok" equals false
    And the JSON at "$.data.failed[0].stem" equals "shop-worker"
    And the JSON at "$.data.failed[0].error.code" equals "START_FAILED"
    And the JSON at "$.data.failed[0].error.details.exit_code" equals 3
    And the JSON at "$.data.ready" equals ["shop-api"]
    And the JSON at "$.data.skipped" equals ["shop-web"]
    And the JSON error has code "START_FAILED"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-worker'].state" equals "failed"
    And the JSON at "$.data.stems[?@.name=='shop-worker'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='shop-web'].state" equals "stopped"
    And the JSON at "$.data.summary.failed" equals 1
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: --no-fail-fast starts everything that does not depend on the crash
    Given the fixture workspace "process-shop"
    And a local override setting stems.shop-worker.env.SHOP_CRASH_ON_START="3"
    When I run "stems up --detach --no-fail-fast --json"
    Then the exit code is 3
    And the JSON at "$.data.failed[0].stem" equals "shop-worker"
    And the JSON at "$.data.ready" equals ["shop-api", "shop-web"]
    And the JSON at "$.data.skipped" equals []
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: nothing ready exits 1
    Given the "minimal" workspace with a local override setting stems.echo-svc.env.SHOP_CRASH_ON_START="2"
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON at "$.data.ready" equals []
    And the JSON at "$.data.failed[0].error.details.exit_code" equals 2
    When I run "stems down --all --json"
    Then the command succeeds
