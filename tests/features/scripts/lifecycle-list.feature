@FR-SC-1
Feature: the lifecycle hooks are listed by stems scripts
  The fixed lifecycle script names (`setup`, `seed`, `build`, `start`,
  `stop`, `reset`, `health`, `pre_start`, `post_start`, `pre_stop`,
  `post_stop`; workspace `bootstrap`, `teardown`) are discoverable from the
  CLI: `stems scripts --lifecycle` lists every hook, set or not, in
  lifecycle order with its scope, when it runs and who defines it, and
  plain `stems scripts --json` carries the names as `lifecycle_hooks`. No
  daemon is needed or started.

  Scenario: hello-shop's hooks, defined or not
    Given the "hello-shop" workspace
    When I run "stems scripts --lifecycle --json"
    Then the command succeeds
    And the JSON nodes at "$.data.lifecycle[*].name" equal ["bootstrap", "setup", "pre_start", "start", "health", "post_start", "seed", "pre_stop", "stop", "post_stop", "build", "reset", "teardown"]
    And the JSON at "$.data.lifecycle[?@.name == 'bootstrap'].scope" equals "workspace"
    And the JSON at "$.data.lifecycle[?@.name == 'bootstrap'].defined_by" equals ["workspace"]
    And the JSON at "$.data.lifecycle[?@.name == 'setup'].scope" equals "stem"
    And the JSON at "$.data.lifecycle[?@.name == 'setup'].defined_by" contains "shop-api"
    And the JSON at "$.data.lifecycle[?@.name == 'setup'].when" exists
    And the JSON at "$.data.lifecycle[?@.name == 'pre_stop'].defined_by" equals []
    And the lock file and socket do not exist
    When I run "stems scripts --json"
    Then the command succeeds
    And the JSON at "$.data.lifecycle_hooks" equals ["bootstrap", "setup", "pre_start", "start", "health", "post_start", "seed", "pre_stop", "stop", "post_stop", "build", "reset", "teardown"]
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].kind" equals "custom"
    And the lock file and socket do not exist
