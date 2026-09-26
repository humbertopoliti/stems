@FR-WS-1
Feature: Workspace discovery
  stems finds stems.yaml via --workspace, STEMS_WORKSPACE, or by walking up
  from the current directory. (Needs `stems show`, deliverable 07.)

  Scenario: running from a subdirectory walks up to the workspace
    Given the "minimal" workspace
    When I run "stems show --json" from "scripts/nested/deeper"
    Then the command succeeds
    And the JSON at "$.data.name" equals "minimal"

  Scenario: STEMS_WORKSPACE names the workspace
    Given the "minimal" workspace
    When I run "stems show --json" with env STEMS_WORKSPACE=${ws}
    Then the command succeeds
    And the JSON at "$.data.name" equals "minimal"

  Scenario: STEMS_WORKSPACE wins over walking up from the cwd
    Given the "minimal" workspace
    When I run "stems show --json" with env STEMS_WORKSPACE=${outside}
    Then the exit code is 2
    And the JSON error has code "WORKSPACE_NOT_FOUND"
    And the error message contains "STEMS_WORKSPACE"

  @error
  Scenario: outside any workspace
    Given the "minimal" workspace
    When I run "stems show --json" from outside any workspace
    Then the exit code is 2
    And the JSON error has code "WORKSPACE_NOT_FOUND"
