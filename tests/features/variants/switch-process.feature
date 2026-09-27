@FR-ST-8 @FR-WS-4
Feature: stems switch changes a stem's variant in stems.local.yaml and applies it
  `stems switch <stem> <variant>` writes `stems.<stem>.variant` to the
  per-developer stems.local.yaml (keeping the harness's port overrides) and,
  with the daemon running, restarts that stem in its new form; `local`
  removes the key again. `stems switch <stem>` lists the variants. No Docker
  needed: `slow` is a process variant of `api` (SHOP_SLEEP_START=1).
  Fixture: tests/fixtures/workspaces/variants-demo.

  Scenario: switch api to a process variant and back
    Given the fixture workspace "variants-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].variant" equals "local"
    And the JSON at "$.data.stems[?@.name=='web'].variant" does not exist
    When I save the JSON at "$.data.stems[?@.name=='api'].pid" as "pid0"
    And I save the JSON at "$.data.stems[?@.name=='web'].pid" as "web_pid"
    When I run "stems switch api slow --json"
    Then the command succeeds
    And the JSON at "$.data.from" equals "local"
    And the JSON at "$.data.to" equals "slow"
    And the JSON at "$.data.type" equals "process"
    And the JSON at "$.data.changed" equals true
    And the JSON at "$.data.daemon" equals true
    And the JSON at "$.data.applied.applied[?@.stem=='api'].result" equals "restarted"
    And the JSON at "$.data.status.variant" equals "slow"
    And the file "stems.local.yaml" contains "variant: slow"
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].variant" equals "slow"
    And the JSON at "$.data.stems[?@.name=='api'].env.SHOP_SLEEP_START" equals "1"
    And the JSON at "$.data.stems[?@.name=='api'].pid" does not equal ${var:pid0}
    And the JSON at "$.data.stems[?@.name=='web'].pid" equals ${var:web_pid}
    When I save the JSON at "$.data.stems[?@.name=='api'].pid" as "pid1"
    When I run "stems switch api local --json"
    Then the command succeeds
    And the JSON at "$.data.to" equals "local"
    And the JSON at "$.data.applied.applied[?@.stem=='api'].result" equals "restarted"
    When I run the shell command "grep -q 'variant:' stems.local.yaml"
    Then the exit code is 1
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].variant" equals "local"
    And the JSON at "$.data.stems[?@.name=='api'].env.SHOP_SLEEP_START" does not exist
    And the JSON at "$.data.stems[?@.name=='api'].pid" does not equal ${var:pid1}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: switch without a variant lists the choices
    Given the fixture workspace "variants-demo"
    When I run "stems switch api local --no-apply --json"
    Then the command succeeds
    And the JSON at "$.data.changed" equals false
    When I run "stems switch api --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "local"
    And the JSON at "$.data.variants[0].name" equals "local"
    And the JSON at "$.data.variants[1].name" equals "slow"
    And the JSON at "$.data.variants[2].name" equals "docker"
    And the JSON at "$.data.variants[?@.name=='docker'].type" equals "docker"
    And the JSON at "$.data.variants[?@.name=='slow'].type" equals "process"
    And the JSON at "$.data.variants[?@.name=='local'].active" equals true
    When I run "stems switch api slow --no-apply --json"
    Then the command succeeds
    And the JSON at "$.data.daemon" equals null
    When I run "stems switch api --json"
    Then the JSON at "$.data.variant" equals "slow"
    And the JSON at "$.data.variants[?@.name=='slow'].active" equals true

  @error
  Scenario: an unknown variant or stem is rejected and nothing is written
    Given the fixture workspace "variants-demo"
    When I run "stems switch api nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_VARIANT" and path "stems.api.variant"
    And the JSON at "$.errors[0].details.known" equals ["slow", "docker"]
    When I run the shell command "grep -q 'variant:' stems.local.yaml"
    Then the exit code is 1
    When I run "stems switch web docker --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_VARIANT"
    When I run "stems switch nobody local --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
