Feature: External stems are never started or stopped
  `up` and `down` leave an external stem alone; until health probes (21)
  it is `unknown`. `start`/`stop` of an external stem fail with
  NOT_MANAGED. Deliverable 13 extends this.

  Scenario: up reports the external stem unknown
    Given the fixture workspace "with-external"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api", "hosted"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='hosted'].state" equals "unknown"
    And the JSON at "$.data.stems[?@.name=='hosted'].glyph" equals "unknown"
    And the JSON at "$.data.stems[?@.name=='hosted'].type" equals "external"
    And the JSON at "$.data.summary.unknown" equals 1

  @error
  Scenario: start and stop of an external stem are NOT_MANAGED
    Given the fixture workspace "with-external"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems start hosted --json"
    Then the exit code is 1
    And the JSON error has code "NOT_MANAGED"
    When I run "stems stop hosted --json"
    Then the exit code is 1
    And the JSON error has code "NOT_MANAGED"
    When I run "stems down --all --json"
    Then the command succeeds
