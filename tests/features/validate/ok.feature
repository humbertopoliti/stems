@FR-WS-6 @FR-GR-2
Feature: Valid workspaces pass validation and report their start order
  `stems validate --json` on a valid workspace exits 0 with `ok: true` and
  the start layers: every stem's hard dependencies are in an earlier layer,
  names are alphabetical within a layer, soft edges do not order starts.
  hello-shop only requires python3, so no tool check is skipped.
  (Needs `stems validate`, deliverable 07.)

  Scenario: hello-shop validates with three start layers
    Given the "hello-shop" workspace
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.ok" equals true
    And the JSON at "$.errors" equals []
    And the JSON at "$.data.start_order" equals [["httpbin", "postgres", "redis"], ["shop-api", "shop-worker"], ["shop-web"]]

  Scenario: minimal validates with a single layer
    Given the "minimal" workspace
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.ok" equals true
    And the JSON at "$.data.start_order" equals [["echo-svc"]]
