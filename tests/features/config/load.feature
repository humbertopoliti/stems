@FR-WS-1 @FR-WS-4 @FR-WS-10 @FR-ST-2 @FR-ST-4 @FR-WS-3
Feature: Loading stems.yaml with a local override
  The resolved config merges stems.local.yaml over stems.yaml, keeps stems
  disabled locally listed with enabled=false, and substitutes ${stem.<n>.port}.
  (Needs `stems show`, deliverable 07.)

  Scenario: a local override disables a stem and ports are substituted
    Given the "hello-shop" workspace with a local override setting stems.shop-web.enabled=false
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.stems['shop-web'].enabled" equals false
    And the JSON at "$.data.stems['shop-api'].enabled" equals true
    And the JSON at "$.data.stems['shop-api'].env.DATABASE_URL" contains "${port:15432}"

  Scenario: an auto port stays a deferred reference
    Given the "hello-shop" workspace
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.stems['shop-web'].ports[0].port" equals "auto"
    And the JSON at "$.data.stems['shop-web'].health.url" contains "${stem.shop-web.port}"
