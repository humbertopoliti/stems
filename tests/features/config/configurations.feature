@FR-WS-4 @FR-WS-6
Feature: named configurations are listed, selected and saved
  `configurations:` in stems.yaml declares named overlays with the shape of
  stems.local.yaml, inline or as a file; `configuration: <name>` in
  stems.local.yaml selects one (merged between stems.yaml and the local
  file, before variants). `stems configuration` lists them, `use` writes
  the selection (and applies it through a running daemon), `save` turns
  the current local overrides into a new configuration file.
  Fixture: tests/fixtures/workspaces/configurations-demo.

  Scenario: list the configurations and their sources
    Given the fixture workspace "configurations-demo"
    When I run "stems configuration --json"
    Then the command succeeds
    And the JSON at "$.data.active" equals null
    And the JSON at "$.data.default" equals null
    And the JSON at "$.data.configurations[0].name" equals "slow"
    And the JSON at "$.data.configurations[0].source" equals "inline"
    And the JSON at "$.data.configurations[0].active" equals false
    And the JSON at "$.data.configurations[1].name" equals "quiet"
    And the JSON at "$.data.configurations[1].source" equals "file"
    And the JSON at "$.data.configurations[1].file" contains "configurations/quiet.yaml"
    When I run "stems show --json"
    Then the JSON at "$.data.configuration" does not exist
    And the JSON at "$.data.configurations[1].source" equals "file"

  Scenario: use with a running daemon applies the configuration's variant
    Given the fixture workspace "configurations-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].variant" equals "local"
    When I save the JSON at "$.data.stems[?@.name=='api'].pid" as "pid0"
    And I save the JSON at "$.data.stems[?@.name=='web'].pid" as "web_pid"
    When I run "stems configuration use slow --json"
    Then the command succeeds
    And the JSON at "$.data.from" equals null
    And the JSON at "$.data.to" equals "slow"
    And the JSON at "$.data.changed" equals true
    And the JSON at "$.data.daemon" equals true
    And the JSON at "$.data.applied.applied[?@.stem=='api'].result" equals "restarted"
    And the file "stems.local.yaml" contains "configuration: slow"
    When I run "stems daemon status --json"
    Then the JSON at "$.data.configuration" equals "slow"
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].variant" equals "slow"
    And the JSON at "$.data.stems[?@.name=='api'].env.SHOP_SLEEP_START" equals "1"
    And the JSON at "$.data.stems[?@.name=='api'].pid" does not equal ${var:pid0}
    And the JSON at "$.data.stems[?@.name=='web'].pid" equals ${var:web_pid}
    When I run "stems configuration list --json"
    Then the JSON at "$.data.active" equals "slow"
    And the JSON at "$.data.configurations[?@.name=='slow'].active" equals true
    When I run "stems configuration use none --json"
    Then the command succeeds
    And the JSON at "$.data.from" equals "slow"
    And the JSON at "$.data.to" equals null
    And the JSON at "$.data.applied.applied[?@.stem=='api'].result" equals "restarted"
    When I run the shell command "grep -q 'configuration:' stems.local.yaml"
    Then the exit code is 1
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].variant" equals "local"
    And the JSON at "$.data.stems[?@.name=='api'].env.SHOP_SLEEP_START" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: use without a daemon only edits stems.local.yaml; the local file still wins
    Given the fixture workspace "configurations-demo"
    When I run "stems configuration use quiet --json"
    Then the command succeeds
    And the JSON at "$.data.to" equals "quiet"
    And the JSON at "$.data.daemon" equals false
    And the JSON at "$.data.applied" equals null
    And the file "stems.local.yaml" contains "configuration: quiet"
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.configuration" equals "quiet"
    And the JSON at "$.data.stems.api.env.FROM" equals "quiet"
    And the JSON at "$.data.stems.web.enabled" equals false
    When I run "stems config set stems.api.env.FROM local --json"
    Then the command succeeds
    When I run "stems show api --json"
    Then the JSON at "$.data.env.FROM" equals "local"
    And the JSON at "$.data.local_env.FROM" equals "local"
    When I run "stems configuration use none --no-apply --json"
    Then the command succeeds
    And the JSON at "$.data.daemon" equals null
    When I run "stems show --json"
    Then the JSON at "$.data.configuration" does not exist
    And the JSON at "$.data.stems.web.enabled" equals true

  Scenario: save the local overrides as a configuration, then use it
    Given the fixture workspace "configurations-demo"
    When I run "stems config set stems.api.env.SAVED yes --json"
    Then the command succeeds
    When I run "stems configuration save mine --json"
    Then the command succeeds
    And the JSON at "$.data.name" equals "mine"
    And the JSON at "$.data.relative" equals "configurations/mine.yaml"
    And the JSON at "$.data.keys" contains "stems"
    And the JSON at "$.data.path" equals "configurations.mine"
    And the JSON at "$.data.changed" equals true
    And the file "configurations/mine.yaml" exists
    And the file "configurations/mine.yaml" contains "SAVED"
    And the file "stems.yaml" contains "mine: configurations/mine.yaml"
    When I run "stems configuration save mine --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"
    When I run "stems configuration list --json"
    Then the JSON at "$.data.active" equals null
    And the JSON at "$.data.configurations[2].name" equals "mine"
    And the JSON at "$.data.configurations[2].source" equals "file"
    When I run "stems config unset stems.api.env.SAVED --json"
    Then the command succeeds
    When I run "stems show api --json"
    Then the JSON at "$.data.env.SAVED" does not exist
    When I run "stems configuration use mine --json"
    Then the command succeeds
    When I run "stems show api --json"
    Then the JSON at "$.data.env.SAVED" equals "yes"
    When I run "stems validate --json"
    Then the command succeeds

  @error
  Scenario: an unknown configuration is rejected and nothing is written
    Given the fixture workspace "configurations-demo"
    When I run "stems configuration use nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_CONFIGURATION" and path "configuration"
    And the JSON at "$.errors[0].details.known" equals ["slow", "quiet"]
    When I run the shell command "grep -q 'configuration:' stems.local.yaml"
    Then the exit code is 1
    Given a local override setting configuration=nope
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_CONFIGURATION" and path "configuration"
    When I run "stems configuration --json"
    Then the exit code is 0
    And the JSON at "$.data.active" equals null
