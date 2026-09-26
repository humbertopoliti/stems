@FR-UI-2
Feature: The command palette
  `Ctrl-P` opens a fuzzy search over stems and actions ("restart
  shop-api", "run seed-large", "view logs api"); `Enter` runs the best
  match.

  Scenario: Ctrl-P, "rest api", Enter restarts shop-api
    Given the fixture workspace "run-scripts"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='shop-api'].pid" as "pid"
    When I run "stems attach --headless --script 'wait:healthy;Ctrl-P;type:rest api;frame;Enter;wait:stem=shop-api:healthy;frame'"
    Then the command succeeds
    And frame 1 contains "› restart shop-api"
    And the last frame contains "✓ restarted shop-api"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].pid" does not equal ${var:pid}
    And within 2s the events stream contains {"kind": "stem.state", "stem": "shop-api", "to": "stopping"}
