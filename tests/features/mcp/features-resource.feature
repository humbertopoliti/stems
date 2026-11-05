@FR-AI-1
Feature: MCP feature index resource
  `stems://features` serves the feature index (the same JSON as `stems
  features --json`) so agents can discover what has no tool; it needs no
  daemon.

  Scenario: read stems://features without a daemon
    Given the "minimal" workspace
    And an MCP client is connected
    When the MCP client reads resource "stems://features"
    Then the resource content contains "one_liner"
    And the resource content contains "stems exec"
    And the resource content contains "docs/ports.md"
