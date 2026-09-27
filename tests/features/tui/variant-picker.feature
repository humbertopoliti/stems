@FR-ST-8 @FR-UI-2
Feature: v switches the selected stem's variant from the dashboard
  `v` opens a picker of the stem's choices (`local`, then its variants,
  the active one marked); `Enter` asks "restart api as slow? [y/N]" and
  `y` calls `switch_variant {stem, variant}`: the daemon writes
  `stems.<stem>.variant` to stems.local.yaml and restarts the stem in its
  new form, as `stems switch` does. No Docker needed: `slow` is a process
  variant of `api` (SHOP_SLEEP_START=1). Fixture:
  tests/fixtures/workspaces/variants-demo.

  Scenario: the picker switches api to slow
    Given the fixture workspace "variants-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='api'].pid" as "pid0"
    When I run "stems attach --view table --headless --script 'wait:healthy;frame;v;frame;j;Enter;frame;y;wait:stem=api:healthy;frame'"
    Then the command succeeds
    And frame 1 contains "v variant local ▸ slow"
    And frame 2 contains "Variant of api"
    And frame 2 contains "› ● local   process  active"
    And frame 2 contains "○ docker  docker"
    And frame 3 contains "restart api as slow? [y/N]"
    And frame 4 contains "✓ api: local -> slow (restarted)"
    And frame 4 contains "v variant slow ▸ docker"
    And the file "stems.local.yaml" contains "variant: slow"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].variant" equals "slow"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].pid" does not equal ${var:pid0}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='config.applied'].actor" contains "tui:"
    When I run "stems down --json"
    Then the command succeeds

  Scenario: v on a stem without variants says so
    Given the fixture workspace "variants-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --view table --headless --script 'wait:healthy;j;v;frame'"
    Then the command succeeds
    And the last frame contains "web has no variants"
    And the last frame does not contain "Variant of"
