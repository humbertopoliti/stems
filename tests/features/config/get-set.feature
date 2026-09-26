@FR-CL-5
Feature: config get / set / unset edit local overrides
  `stems config set <path> <value>` writes stems.local.yaml in place, keeping
  its comments and layout, and validates the workspace before keeping the
  change; `config get` prints the resolved value and where it came from;
  `config unset` removes a local value. No daemon is involved.

  Background:
    Given the "minimal" workspace with its original ports
    And the local override file contains:
      """
      # my machine only (this comment must survive)
      stems:
        echo-svc:
          env:
            SHOP_CHAOS: "0"  # quieter locally
      """

  Scenario: set writes the local file preserving comments, get reports the source
    When I run "stems config set stems.echo-svc.enabled false --json"
    Then the command succeeds
    And the JSON at "$.data.changed" equals true
    And the JSON at "$.data.diff" equals ["+    enabled: false"]
    And the file "stems.local.yaml" contains "# my machine only (this comment must survive)"
    And the file "stems.local.yaml" contains "SHOP_CHAOS: "0"  # quieter locally"
    And the file "stems.local.yaml" contains "    enabled: false"
    When I run "stems config get stems.echo-svc.enabled --json"
    Then the command succeeds
    And the JSON at "$.data.value" equals false
    And the JSON at "$.data.source" equals "stems.local.yaml"
    When I run "stems config get stems.echo-svc.env.PORT --json"
    Then the JSON at "$.data.value" equals "18090"
    And the JSON at "$.data.source" equals "stems.yaml"
    When I run "stems config get stems.echo-svc.restart.max --json"
    Then the JSON at "$.data.source" equals "default"

  Scenario: set replaces an existing value and keeps its trailing comment
    When I run "stems config set stems.echo-svc.env.SHOP_CHAOS 1 --json"
    Then the command succeeds
    And the JSON at "$.data.diff" equals ["-      SHOP_CHAOS: \"0\"  # quieter locally", "+      SHOP_CHAOS: 1  # quieter locally"]
    When I run "stems config get stems.echo-svc.env.SHOP_CHAOS --json"
    Then the JSON at "$.data.value" equals "1"

  Scenario: unset removes the local value
    When I run "stems config set stems.echo-svc.enabled false --json"
    And I run "stems config unset stems.echo-svc.enabled --json"
    Then the command succeeds
    And the JSON at "$.data.diff" equals ["-    enabled: false"]
    When I run "stems config get stems.echo-svc.enabled --json"
    Then the JSON at "$.data.value" equals true
    And the JSON at "$.data.source" equals "default"
    And the file "stems.local.yaml" contains "# my machine only (this comment must survive)"

  @error
  Scenario: an invalid value is rejected and the file is left unchanged
    When I run "stems config set stems.echo-svc.ports[0].port abc --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID"
    And the file "stems.local.yaml" contains "# my machine only (this comment must survive)"
    When I run "stems config get stems.echo-svc.ports[0].port --json"
    Then the JSON at "$.data.value" equals 18090
    And the JSON at "$.data.source" equals "stems.yaml"

  @error
  Scenario: a value that breaks validation is rejected
    When I run "stems config set profile nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_PROFILE"
    When I run "stems config get profile --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"

  @error
  Scenario: a typo in the path is rejected
    When I run "stems config set stems.echo-svc.enabld false --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID"
    And the file "stems.local.yaml" contains "quieter locally"

  Scenario: set creates stems.local.yaml when it is missing
    When I run the shell command "rm stems.local.yaml"
    And I run "stems config set stems.echo-svc.stop_grace 1s --json"
    Then the command succeeds
    And the file "stems.local.yaml" contains "stop_grace: 1s"
    When I run "stems config get stems.echo-svc.stop_grace --json"
    Then the JSON at "$.data.source" equals "stems.local.yaml"
