@NFR-8 @NFR-5 @FR-DS-1 @FR-DS-2
Feature: Harness smoke test
  The harness drives the real, single `stems` binary (FR-DS-1 binary layout)
  in an isolated copy of an example workspace and asserts on --json output
  only. CI runs it on macOS and Linux (NFR-5).

  Scenario: the binary reports its version as JSON
    Given the "minimal" workspace
    When I run "stems --version --json"
    Then the command succeeds
    And the JSON at "$.version" matches semver
