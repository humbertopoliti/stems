@FR-ST-4
Feature: stems env prints a stem's environment
  `stems env <stem>` prints what stems sets for a stem (dotenv by default,
  `--format shell` for eval, `--json`), from the daemon or, without one,
  from the config (deliverable 37).

  Scenario: json and shell formats without a daemon
    Given the fixture workspace "caller-env"
    When I run "stems env api --json"
    Then the command succeeds
    And the JSON at "$.data.env.PORT" equals "${port:18791}"
    And the JSON at "$.data.env.GREETING" equals "hello"
    And the JSON at "$.data.env.STEMS_STEM" equals "api"
    And the JSON at "$.data.container" equals null
    And the JSON at "$.data.unresolved" equals []
    When I run "stems env api --format shell"
    Then the command succeeds
    And stdout contains "export GREETING='hello'"
    And stdout contains "export PORT='${port:18791}'"
    When I run "stems env worker --json"
    Then the JSON at "$.data.unresolved" equals ["PORT", "PORT_HTTP", "URL"]
    And the JSON at "$.data.env.PORT" does not exist
    And the JSON at "$.data.env.PORT_HTTP" does not exist
    And the JSON at "$.data.env.URL" contains "${stem."
    # A dependant of the auto port: its STEMS_<DEP>_PORT is unresolved too.
    When I run "stems env client --json"
    Then the JSON at "$.data.unresolved" equals ["STEMS_WORKER_PORT", "STEMS_WORKER_PORT_HTTP", "WORKER_URL"]
    And the JSON at "$.data.env.WORKER_URL" contains "${stem.worker."
    And the JSON at "$.data.env.STEMS_WORKER_PORT" does not exist
    When I run "stems daemon status --json"
    Then the JSON at "$.data.running" equals false

  Scenario: a running stem's env comes from the daemon
    Given the fixture workspace "caller-env"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems env worker --json"
    Then the command succeeds
    And the JSON at "$.data.unresolved" equals []
    And the JSON at "$.data.env.URL" contains "http://localhost:"
    And the JSON at "$.data.env.PORT" exists
    And the JSON at "$.data.env.STEMS_RUN_ID" exists
    When I run "stems down --all --json"
    Then the command succeeds
