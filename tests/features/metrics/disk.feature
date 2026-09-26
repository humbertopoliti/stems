@FR-MT-3
Feature: --disk measures build outputs on demand
  `stems metrics --disk` walks the codebase's build-output directories
  (`target`, `dist`, `build`, `node_modules`, `.venv`) and, for docker
  stems, sums their named volumes (`docker system df`). It is slow, so it
  only runs when asked and is cached for 60 s.

  Scenario: a dist/ directory in the codebase is counted
    Given the "minimal" workspace
    And the workspace has a private copy of the repos
    When I run the shell command "mkdir -p ${tmp}/examples/repos/shop-api/dist/js && head -c 1100000 /dev/zero > ${tmp}/examples/repos/shop-api/dist/js/bundle.js"
    And I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems metrics --disk --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].disk.codebase_build_bytes" is greater than 1000000
    And the JSON at "$.data.stems[0].disk.dirs[0].path" contains "dist"
    And the JSON at "$.data.totals.disk_bytes" is greater than 1000000
    When I run "stems metrics --json"
    Then the JSON at "$.data.stems[0].disk" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
