@FR-CL-1
Feature: Commands of later deliverables exist as stubs
  The whole command surface exists from deliverable 07. A command whose
  deliverable has not landed exits 1 with NOT_IMPLEMENTED and names the
  deliverable in `details.deliverable`. Remove a scenario from here (or
  change it) when its deliverable implements the command.

  @error
  Scenario: stems mcp is not implemented yet
    Given the "minimal" workspace
    When I run "stems mcp --json"
    Then the exit code is 1
    And the JSON at "$.ok" equals false
    And the JSON error has code "NOT_IMPLEMENTED"
    And the JSON at "$.errors[0].details.deliverable" equals "31"
