@FR-SC-6
Feature: workspace scripts and `requires:`
  `stems run --ws <script>` runs a workspace-level script. A script's
  `requires:` stems must be healthy: otherwise `SCRIPT_REQUIRES_UNMET`, or
  `--start-deps` starts them first.

  Scenario: an unmet requirement is refused
    Given the fixture workspace "run-scripts"
    And the daemon is started
    When I run "stems run --ws needs-api --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_REQUIRES_UNMET"
    And the JSON at "$.errors[0].details.unmet[0].stem" equals "shop-api"
    And the JSON at "$.errors[0].details.unmet[0].state" equals "stopped"
    And the JSON at "$.errors[0].hint" contains "--start-deps"
    And the events stream does not contain {"kind": "script.started"}

  Scenario: --start-deps starts the required stems first
    Given the fixture workspace "run-scripts"
    When I run "stems run --ws needs-api --start-deps --json"
    Then the command succeeds
    And the JSON at "$.data.stem" equals null
    And the JSON at "$.data.tail[0]" contains "needs-api: shop-api is at"
    And the events stream contains {"kind": "stem.state", "stem": "shop-api", "to": "healthy"} before {"kind": "script.started", "data": {"script": "needs-api"}}
    When I run "stems logs _workspace --script needs-api --json"
    Then the JSON at "$..text" contains "needs-api: shop-api is at"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a met requirement runs at once; --workspace-script is an alias
    Given the fixture workspace "run-scripts"
    When I run "stems up shop-api --detach --json"
    Then the command succeeds
    When I run "stems run --workspace-script needs-api --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    When I run "stems down --all --json"
    Then the command succeeds
